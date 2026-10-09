// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Which engine a game folder holds, which program is the game, and whether it carries
//! anti-cheat. Read-only. The walk is bounded (depth 4, 5000 entries) and never follows a link.
//!
//! A program is the game by what sits beside it, never by its name or by which game it is:
//!
//! 1. **Steam's launch config.** The program Steam launches
//!    ([`crate::steam_appinfo::launch_executable`], passed to [`detect_with`]) wins outright
//!    when it passes rule 2 or 5.
//! 2. **Unity.** `<stem>_Data/` and `UnityPlayer.dll` (or `UnityPlayer.so`) in the program's own
//!    folder: `MiSideFull.exe` ↔ `MiSideFull_Data`.
//! 3. **Unity's flavour, from that same folder.** `GameAssembly.dll`/`.so` beside the program is
//!    IL2CPP; `<stem>_Data/Managed/Assembly-CSharp.dll` is Mono.
//! 4. **Several pass.** The shallowest (by where its evidence sits), then the one Steam
//!    launches, then the largest evidence (a bounded walk).
//! 5. **The same idea per engine.** Unreal: `<Project>/Binaries/Win64/<Name>-Win64-Shipping.exe`
//!    with `<Project>/Content/Paks/`. Godot: `<stem>.pck` beside the program.
//!
//! Weaker signs, each at Low confidence: a Unity game from before 2017.2 (its player linked into
//! the program, so no `UnityPlayer`, but `<stem>_Data/Managed/Assembly-CSharp.dll`) ranks with
//! the programs that pass, by depth, after them at the same depth. Only when no game is found at
//! all do the rest speak: half an Unreal pairing, a Godot pck embedded in the program.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result};
use crate::paths::relative_components;

const MAX_DEPTH: usize = 4;
const MAX_ENTRIES: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Engine {
    UnityMono,
    UnityIl2cpp,
    Unreal,
    Godot,
    Unknown,
}

impl Engine {
    pub fn code(self) -> &'static str {
        match self {
            Engine::UnityMono => "unity-mono",
            Engine::UnityIl2cpp => "unity-il2cpp",
            Engine::Unreal => "unreal",
            Engine::Godot => "godot",
            Engine::Unknown => "unknown",
        }
    }

    pub fn from_code(code: &str) -> Option<Engine> {
        Some(match code {
            "unity-mono" => Engine::UnityMono,
            "unity-il2cpp" => Engine::UnityIl2cpp,
            "unreal" => Engine::Unreal,
            "godot" => Engine::Godot,
            "unknown" => Engine::Unknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    High,
    Low,
}

/// Which kind of program the game ships. A Windows exe on Linux runs under Proton.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Binary {
    Windows,
    Linux,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detection {
    pub engine: Engine,
    pub confidence: Confidence,
    /// The game's program, relative to the folder detected.
    pub exe: Option<PathBuf>,
    pub binary: Binary,
    /// Codes: "easy-anti-cheat", "battleye", "gameguard", "xigncode", "hoyo-protect".
    pub anti_cheat: Vec<String>,
    /// What made the call, relative to the folder detected: the `<stem>_Data` folder (Unity),
    /// the `<Project>` folder (Unreal), the `.pck` (Godot; the program itself when the pck is
    /// embedded in it). `None` when nothing did. For support: it says WHY.
    #[serde(default)]
    pub evidence: Option<PathBuf>,
    /// The launch hint given to [`detect_with`] passed the rules and is the program detected.
    /// `false` when there was none or it was ignored (outside the folder, missing, a launcher).
    #[serde(default)]
    pub hint_used: bool,
    /// The Unity version the game was built with (`2021.3.16f1`, `6000.3.1f1`), read from the
    /// header of `<stem>_Data/globalgamemanagers` or `data.unity3d`. Unity games only; `None`
    /// when neither file says. Some loaders break on some Unity lines, and this is how a
    /// frontend tells them apart before anything is installed. See [`unity_version_parts`].
    #[serde(default)]
    pub unity_version: Option<String>,
}

/// The `(major, minor)` of a Unity version string: `6000.3.1f1` → `(6000, 3)`.
pub fn unity_version_parts(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// How much of a data file's head is read for its version string. Both headers put it in the
/// first hundred bytes; the margin is for formats this was not shown.
const VERSION_HEAD: u64 = 4096;

/// The Unity version in a game's data folder, from the first file that names one.
fn unity_version_in(data_dir: &Path) -> Option<String> {
    ["globalgamemanagers", "data.unity3d", "mainData"]
        .iter()
        .find_map(|name| {
            let mut head = Vec::new();
            File::open(data_dir.join(name))
                .ok()?
                .take(VERSION_HEAD)
                .read_to_end(&mut head)
                .ok()?;
            find_unity_version(&head)
        })
}

/// The first `<digits>.<digits>.<digits><a|b|f|p|x><digits>` in `bytes`. The letter is required:
/// a bundle header also carries its format as `5.x.x`, which is not the engine's version.
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

struct Entry {
    /// Components below the root, original case.
    rel: Vec<String>,
    is_dir: bool,
}

impl Entry {
    fn name(&self) -> &str {
        self.rel.last().map(String::as_str).unwrap_or("")
    }

    fn path(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        p.extend(&self.rel);
        p
    }
}

/// Breadth first, sorted, bounded, links skipped.
fn walk(root: &Path) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut queue: VecDeque<(PathBuf, Vec<String>)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), Vec::new()));
    while let Some((dir, rel)) = queue.pop_front() {
        let read = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if rel.is_empty() => return Err(LauncherError::io_at(&e, &dir)),
            Err(_) => continue,
        };
        let mut items: Vec<(String, bool)> = Vec::new();
        for item in read.flatten() {
            let Ok(ft) = item.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            items.push((item.file_name().to_string_lossy().into_owned(), ft.is_dir()));
        }
        items.sort();
        for (name, is_dir) in items {
            if out.len() >= MAX_ENTRIES {
                return Ok(out);
            }
            let mut child = rel.clone();
            child.push(name);
            if is_dir && child.len() < MAX_DEPTH {
                queue.push_back((dir.join(child.last().unwrap()), child.clone()));
            }
            out.push(Entry { rel: child, is_dir });
        }
    }
    Ok(out)
}

fn anti_cheat_of(entries: &[Entry]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for e in entries {
        let n = e.name().to_ascii_lowercase();
        let kind = if e.is_dir {
            match n.as_str() {
                "easyanticheat" => Some("easy-anti-cheat"),
                "battleye" => Some("battleye"),
                "gameguard" => Some("gameguard"),
                "xigncode" => Some("xigncode"),
                _ => None,
            }
        } else if n == "easyanticheat_eos_setup.exe" || n == "start_protected_game.exe" {
            Some("easy-anti-cheat")
        } else if n.starts_with("beservice") && n.ends_with(".exe") {
            Some("battleye")
        } else if n == "mhypbase.dll" || n == "hoyokprotect.sys" {
            // HoYoverse's protection (Genshin Impact, Honkai: Star Rail, Zenless Zone Zero).
            Some("hoyo-protect")
        } else {
            None
        };
        if let Some(k) = kind
            && !found.iter().any(|f| f == k)
        {
            found.push(k.to_owned());
        }
    }
    found
}

/// The anti-cheat a game folder carries.
pub(crate) fn anti_cheat_scan(game_dir: &Path) -> Result<Vec<String>> {
    Ok(anti_cheat_of(&walk(game_dir)?))
}

fn is_file(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
}

fn is_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// Bounds of the walk that sizes one evidence folder, for the tie-break between two programs
/// that both pass at the same depth.
const SIZE_MAX_ENTRIES: usize = 50_000;
const SIZE_MAX_DEPTH: usize = 16;

/// The names in one folder, each with whether it is a folder. Links are left out.
type Listing = [(String, bool)];

/// What one program says about itself, by the files beside it.
struct Candidate {
    engine: Engine,
    confidence: Confidence,
    exe: Option<Vec<String>>,
    binary: Binary,
    evidence: Option<Vec<String>>,
    /// It passes rule 2 or 5. A weaker sign (a pre-2017.2 Unity game) ranks after one that
    /// passes at the same depth, never before a deeper one.
    passes: bool,
    /// The program's name repeats its evidence's: Unreal's `<Project>-Win64-Shipping.exe` in
    /// `<Project>/` (Palworld's `Pal/…/Palworld-Win64-Shipping.exe` does not). A tie-break only.
    named_alike: bool,
}

impl Candidate {
    fn into_detection(
        self,
        game_dir: &Path,
        anti_cheat: Vec<String>,
        hint_used: bool,
    ) -> Detection {
        let unity_version = match self.engine {
            Engine::UnityMono | Engine::UnityIl2cpp => self
                .evidence
                .as_deref()
                .and_then(|data| unity_version_in(&game_dir.join(rel_path(data)))),
            _ => None,
        };
        Detection {
            unity_version,
            engine: self.engine,
            confidence: self.confidence,
            exe: self.exe.as_deref().map(rel_path),
            binary: self.binary,
            anti_cheat,
            evidence: self.evidence.as_deref().map(rel_path),
            hint_used,
        }
    }

    /// How deep the game sits: its evidence's depth, so an Unreal project at the top
    /// (`<Project>/Binaries/Win64/…`) counts as deep as a Unity game at the top.
    fn depth(&self) -> usize {
        self.evidence
            .as_ref()
            .or(self.exe.as_ref())
            .map_or(usize::MAX, Vec::len)
    }
}

fn joined(parent: &[String], name: &str) -> Vec<String> {
    let mut v = parent.to_vec();
    v.push(name.to_owned());
    v
}

fn abs(root: &Path, rel: &[String]) -> PathBuf {
    let mut p = root.to_path_buf();
    p.extend(rel);
    p
}

/// `name` in `listing`, of the kind asked for: the exact spelling first, else ignoring ASCII
/// case (a game built on Windows may not match its own spelling on a Linux disk).
fn find_in<'a>(listing: &'a Listing, name: &str, dir: bool) -> Option<&'a str> {
    let mut it = listing.iter().filter(|(_, d)| *d == dir);
    it.clone()
        .find(|(n, _)| n == name)
        .or_else(|| it.find(|(n, _)| n.eq_ignore_ascii_case(name)))
        .map(|(n, _)| n.as_str())
}

/// One folder's listing, read from disk. `None` when it cannot be read.
fn listing_of(dir: &Path) -> Option<Vec<(String, bool)>> {
    let mut out = Vec::new();
    for item in fs::read_dir(dir).ok()?.flatten() {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let Ok(ft) = item.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        out.push((item.file_name().to_string_lossy().into_owned(), ft.is_dir()));
    }
    out.sort();
    Some(out)
}

/// The child of `dir` named `name` (exact spelling first, else ignoring ASCII case) that is a
/// real folder or a real file as asked, never a link.
fn child(dir: &Path, name: &str, want_dir: bool) -> Option<String> {
    let kind = |p: &Path| if want_dir { is_dir(p) } else { is_file(p) };
    if kind(&dir.join(name)) {
        return Some(name.to_owned());
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .take(MAX_ENTRIES)
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.eq_ignore_ascii_case(name) && kind(&dir.join(n)))
        .min()
}

/// The stem a program's `_Data` folder or `.pck` repeats: `Game.exe`, `Game.x86_64`,
/// `Game.x86` → `Game`; any other name is its own stem (`Game` on Linux).
fn program_stem(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _)) if is_program_name(name) => stem,
        _ => name,
    }
}

/// Rule 2 and 3, Unity: `<stem>_Data/` and `UnityPlayer.dll`/`.so` beside the program; the
/// flavour from the same folder.
fn unity(game_dir: &Path, parent: &[String], listing: &Listing, name: &str) -> Option<Candidate> {
    let stem = program_stem(name);
    if stem.is_empty() {
        return None;
    }
    let data = find_in(listing, &format!("{stem}_Data"), true)?;
    let has = |n: &str| find_in(listing, n, false).is_some();
    if !has("UnityPlayer.dll") && !has("UnityPlayer.so") {
        return None;
    }
    let data_rel = joined(parent, data);
    let data_path = abs(game_dir, &data_rel);
    let managed = data_path.join("Managed");
    let (engine, confidence) = if has("GameAssembly.dll") || has("GameAssembly.so") {
        (Engine::UnityIl2cpp, Confidence::High)
    } else if is_file(&managed.join("Assembly-CSharp.dll")) {
        (Engine::UnityMono, Confidence::High)
    } else if is_dir(&data_path.join("il2cpp_data")) {
        (Engine::UnityIl2cpp, Confidence::Low)
    } else if is_dir(&managed) {
        // Mono with all its code in other assemblies.
        (Engine::UnityMono, Confidence::Low)
    } else {
        // A Unity player and its data, but neither flavour: the program still IS the game.
        (Engine::Unknown, Confidence::Low)
    };
    Some(Candidate {
        engine,
        confidence,
        exe: Some(joined(parent, name)),
        binary: if ext_lower(name) == "exe" {
            Binary::Windows
        } else {
            Binary::Linux
        },
        evidence: Some(data_rel),
        passes: true,
        named_alike: true,
    })
}

/// Before Unity 2017.2 the player was linked into the program, so there is no `UnityPlayer`
/// and rule 2 cannot pass; `<stem>_Data/Managed/Assembly-CSharp.dll` still proves a Mono game.
fn legacy_unity(
    game_dir: &Path,
    parent: &[String],
    listing: &Listing,
    name: &str,
) -> Option<Candidate> {
    let stem = program_stem(name);
    if stem.is_empty() {
        return None;
    }
    let data = find_in(listing, &format!("{stem}_Data"), true)?;
    let data_rel = joined(parent, data);
    let csharp = abs(game_dir, &data_rel)
        .join("Managed")
        .join("Assembly-CSharp.dll");
    if !is_file(&csharp) {
        return None;
    }
    Some(Candidate {
        engine: Engine::UnityMono,
        confidence: Confidence::Low,
        exe: Some(joined(parent, name)),
        binary: if ext_lower(name) == "exe" {
            Binary::Windows
        } else {
            Binary::Linux
        },
        evidence: Some(data_rel),
        passes: false,
        named_alike: true,
    })
}

/// Rule 5, Godot: `<stem>.pck` beside the program.
fn godot(parent: &[String], listing: &Listing, name: &str) -> Option<Candidate> {
    if !is_program_name(name) {
        return None;
    }
    let pck = find_in(listing, &format!("{}.pck", program_stem(name)), false)?;
    Some(Candidate {
        engine: Engine::Godot,
        confidence: Confidence::High,
        exe: Some(joined(parent, name)),
        binary: binary_of(name),
        evidence: Some(joined(parent, pck)),
        passes: true,
        named_alike: true,
    })
}

/// Rule 5, Unreal: `<Project>/Binaries/Win64/<Name>-Win64-Shipping.exe`. High with
/// `<Project>/Content/Paks/`, Low without it. The name is not required to repeat the project's
/// (Palworld ships `Pal/Binaries/Win64/Palworld-Win64-Shipping.exe`); the pairing is the folder.
fn unreal(game_dir: &Path, parent: &[String], name: &str) -> Option<Candidate> {
    let lower = name.to_ascii_lowercase();
    let prefix = lower.strip_suffix("-win64-shipping.exe")?;
    let n = parent.len();
    if prefix.is_empty()
        || n < 3
        || !parent[n - 1].eq_ignore_ascii_case("Win64")
        || !parent[n - 2].eq_ignore_ascii_case("Binaries")
    {
        return None;
    }
    let project = &parent[..n - 2];
    let project_path = abs(game_dir, project);
    let paks = child(&project_path, "Content", true)
        .and_then(|c| child(&project_path.join(c), "Paks", true))
        .is_some();
    Some(Candidate {
        engine: Engine::Unreal,
        confidence: if paks {
            Confidence::High
        } else {
            Confidence::Low
        },
        exe: Some(joined(parent, name)),
        binary: Binary::Windows,
        evidence: Some(project.to_vec()),
        passes: paks,
        named_alike: project
            .last()
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix)),
    })
}

/// Whether the program at `parent/name` passes rule 2 (Unity) or rule 5 (Unreal, Godot).
fn passes(game_dir: &Path, parent: &[String], listing: &Listing, name: &str) -> Option<Candidate> {
    unity(game_dir, parent, listing, name)
        .or_else(|| godot(parent, listing, name))
        .or_else(|| unreal(game_dir, parent, name).filter(|c| c.passes))
}

/// The bytes below `p`, by a bounded walk that never follows a link. A tie-break, so a walk cut
/// short by its bounds still answers.
fn evidence_size(p: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(p) else {
        return 0;
    };
    if !m.is_dir() {
        return if m.is_file() { m.len() } else { 0 };
    }
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::from([(p.to_path_buf(), 0)]);
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for item in read.flatten() {
            seen += 1;
            if seen > SIZE_MAX_ENTRIES {
                return total;
            }
            let Ok(ft) = item.file_type() else { continue };
            if ft.is_dir() && depth + 1 < SIZE_MAX_DEPTH {
                queue.push_back((item.path(), depth + 1));
            } else if ft.is_file() {
                total = total.saturating_add(item.metadata().map_or(0, |m| m.len()));
            }
        }
    }
    total
}

/// Rule 4: of several programs, the shallowest; then one that passes over a weaker sign; then
/// the largest evidence; then the one whose name repeats its evidence's; then the first by path.
/// (Steam's own pick sits before the size in the owner's order, but a launch hint that passes
/// has already won outright by then.)
fn best(game_dir: &Path, mut cands: Vec<Candidate>) -> Option<Candidate> {
    let shallowest = cands.iter().map(Candidate::depth).min()?;
    cands.retain(|c| c.depth() == shallowest);
    if cands.iter().any(|c| c.passes) {
        cands.retain(|c| c.passes);
    }
    if cands.len() < 2 {
        return cands.pop();
    }
    // Two programs may share one `_Data` (`Game.exe` and `Game.x86_64`): size each folder once.
    let mut sizes: Vec<(Vec<String>, u64)> = Vec::new();
    let mut keyed: Vec<(u64, Candidate)> = Vec::new();
    for c in cands {
        let size = match &c.evidence {
            None => 0,
            Some(e) => match sizes.iter().find(|(k, _)| k == e) {
                Some((_, s)) => *s,
                None => {
                    let s = evidence_size(&abs(game_dir, e));
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
    keyed.into_iter().next().map(|(_, c)| c)
}

/// Rule 1: the program Steam launches, if it is inside the folder and passes rule 2 or 5.
/// Jailed: relative, no `..`, no link on the way; matched ignoring ASCII case like Windows.
fn hinted(game_dir: &Path, hint: &str) -> Option<Candidate> {
    let mut h = hint.trim();
    while let Some(rest) = h.strip_prefix("./").or_else(|| h.strip_prefix(".\\")) {
        h = rest;
    }
    let wanted = relative_components(h)?;
    let mut real: Vec<String> = Vec::with_capacity(wanted.len());
    for (i, c) in wanted.iter().enumerate() {
        let is_last = i + 1 == wanted.len();
        real.push(child(&abs(game_dir, &real), c, !is_last)?);
    }
    let (name, parent) = real.split_last()?;
    let listing = listing_of(&abs(game_dir, parent))?;
    passes(game_dir, parent, &listing, name)
}

/// Detects the engine of a game folder. Read-only. The same as [`detect_with`] without a hint.
pub fn detect(game_dir: &Path) -> Result<Detection> {
    detect_with(game_dir, None)
}

/// Detects the engine of a game folder, given the program Steam launches for it when known
/// (`launch_hint`, relative to `game_dir`, from [`crate::steam_appinfo::launch_executable`]).
/// Read-only.
///
/// A hint inside the folder that passes the Unity, Unreal or Godot rule wins outright and
/// [`Detection::hint_used`] says so. Any other hint (missing, outside the folder, a launcher)
/// is ignored, and `hint_used` is `false`.
pub fn detect_with(game_dir: &Path, launch_hint: Option<&str>) -> Result<Detection> {
    let entries = walk(game_dir)?;
    let anti_cheat = anti_cheat_of(&entries);

    if let Some(c) = launch_hint.and_then(|h| hinted(game_dir, h)) {
        return Ok(c.into_detection(game_dir, anti_cheat, true));
    }

    // Every folder's listing, as the walk saw it.
    let mut folders: HashMap<&[String], Vec<(String, bool)>> = HashMap::new();
    for e in &entries {
        if let Some((name, parent)) = e.rel.split_last() {
            folders
                .entry(parent)
                .or_default()
                .push((name.clone(), e.is_dir));
        }
    }
    // Each file with its folder's listing, for the rules to look beside it.
    let files = || {
        entries.iter().filter(|e| !e.is_dir).filter_map(|e| {
            let (name, parent) = e.rel.split_last()?;
            Some((parent, folders.get(parent)?.as_slice(), name.as_str()))
        })
    };

    // Every program that passes, and every pre-2017.2 Unity game: ranked together by depth, so
    // an old game at the top still beats a newer Unity tool in a subfolder.
    let games: Vec<Candidate> = files()
        .filter(|(_, listing, name)| {
            is_program_name(name) || find_in(listing, &format!("{name}_Data"), true).is_some()
        })
        .filter_map(|(parent, listing, name)| {
            passes(game_dir, parent, listing, name)
                .or_else(|| legacy_unity(game_dir, parent, listing, name))
        })
        .collect();
    if let Some(c) = best(game_dir, games) {
        return Ok(c.into_detection(game_dir, anti_cheat, false));
    }

    // No game found. The weaker signs, each Low.
    // Unreal, half a pairing: the shipping program without `Content/Paks`, then any shipping
    // program at all, then a `Content/Paks/*.pak` with no program.
    let half: Vec<Candidate> = files()
        .filter_map(|(parent, _, name)| unreal(game_dir, parent, name))
        .collect();
    if let Some(c) = best(game_dir, half) {
        return Ok(c.into_detection(game_dir, anti_cheat, false));
    }
    let shipping = entries.iter().find(|e| {
        !e.is_dir
            && e.name()
                .to_ascii_lowercase()
                .ends_with("-win64-shipping.exe")
    });
    let pak_project = entries.iter().find_map(|e| {
        let n = e.rel.len();
        (!e.is_dir
            && n >= 4
            && ext_lower(e.name()) == "pak"
            && e.rel[n - 2].eq_ignore_ascii_case("Paks")
            && e.rel[n - 3].eq_ignore_ascii_case("Content"))
        .then(|| e.rel[..n - 3].to_vec())
    });
    if shipping.is_some() || pak_project.is_some() {
        return Ok(Candidate {
            engine: Engine::Unreal,
            confidence: Confidence::Low,
            exe: shipping.map(|e| e.rel.clone()),
            binary: if shipping.is_some() {
                Binary::Windows
            } else {
                Binary::Unknown
            },
            evidence: if shipping.is_some() {
                None
            } else {
                pak_project
            },
            passes: false,
            named_alike: false,
        }
        .into_detection(game_dir, anti_cheat, false));
    }

    // Godot with the pck embedded in the program.
    let top_programs: Vec<&Entry> = entries
        .iter()
        .filter(|e| !e.is_dir && e.rel.len() == 1 && is_program_name(e.name()))
        .collect();
    if let Some(exe) = top_programs
        .iter()
        .take(8)
        .find(|e| has_embedded_pck(&e.path(game_dir)))
    {
        return Ok(Candidate {
            engine: Engine::Godot,
            confidence: Confidence::Low,
            exe: Some(exe.rel.clone()),
            binary: binary_of(exe.name()),
            evidence: Some(exe.rel.clone()),
            passes: false,
            named_alike: true,
        }
        .into_detection(game_dir, anti_cheat, false));
    }

    // Unknown. Name the program when there is exactly one obvious one.
    let main: Vec<&&Entry> = top_programs
        .iter()
        .filter(|e| !e.name().to_ascii_lowercase().contains("crashhandler"))
        .collect();
    let (exe, binary) = match main.as_slice() {
        [one] => (Some(rel_path(&one.rel)), binary_of(one.name())),
        _ => (None, Binary::Unknown),
    };
    Ok(Detection {
        engine: Engine::Unknown,
        confidence: Confidence::Low,
        exe,
        binary,
        anti_cheat,
        evidence: None,
        hint_used: false,
        unity_version: None,
    })
}

/// Godot keeps an embedded pck at the end of the exe, closed by the magic `GDPC`.
fn has_embedded_pck(p: &Path) -> bool {
    let Ok(mut f) = File::open(p) else {
        return false;
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return false;
    };
    if len < 12 || f.seek(SeekFrom::End(-4)).is_err() {
        return false;
    }
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).is_ok() && &magic == b"GDPC"
}

fn ext_lower(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_program_name(name: &str) -> bool {
    matches!(ext_lower(name).as_str(), "exe" | "x86_64" | "x86")
}

fn binary_of(name: &str) -> Binary {
    match ext_lower(name).as_str() {
        "exe" => Binary::Windows,
        "x86_64" | "x86" => Binary::Linux,
        _ => Binary::Unknown,
    }
}

fn rel_path(rel: &[String]) -> PathBuf {
    rel.iter().collect()
}

/// The engine of the game whose program is `exe_rel`, by that program's own folder: the rules
/// above, then the weaker signs for that one program. `Unknown` when neither speaks: another
/// program in the folder being a game says nothing about this one.
pub(crate) fn engine_of_exe(game_dir: &Path, exe_rel: &[String]) -> Result<Engine> {
    let Some((name, parent)) = exe_rel.split_last() else {
        return Ok(Engine::Unknown);
    };
    let Some(listing) = listing_of(&abs(game_dir, parent)) else {
        return Ok(Engine::Unknown);
    };
    let found = passes(game_dir, parent, &listing, name)
        .or_else(|| legacy_unity(game_dir, parent, &listing, name))
        .or_else(|| unreal(game_dir, parent, name))
        .map(|c| c.engine)
        .or_else(|| {
            name.to_ascii_lowercase()
                .ends_with("-win64-shipping.exe")
                .then_some(Engine::Unreal)
        })
        .or_else(|| {
            (is_program_name(name) && has_embedded_pck(&abs(game_dir, exe_rel)))
                .then_some(Engine::Godot)
        });
    Ok(found.unwrap_or(Engine::Unknown))
}
