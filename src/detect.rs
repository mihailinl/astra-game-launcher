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
//!    with `<Project>/Content/Paks/`. A project that ships no `-Win64-Shipping.exe` (GTA III –
//!    The Definitive Edition: `Gameface/Binaries/Win64/LibertyCity.exe`) has its program in that
//!    same `Binaries/Win64/`: the one Steam launches when the hint names one, else the largest
//!    there, and [`Detection::exe_pick`] says which. `<Project>` itself may sit one package level
//!    deeper (`<root>/<Package>/<Project>/Binaries/Win64/…`; Poppy Playtime's `WindowsNoEditor`,
//!    or any other immediate folder) — the walk looks one level past its normal bound exactly
//!    there, nowhere else, so this costs no unbounded recursion. Only `<Project>`'s own folder is
//!    looked in, so a bootstrapper at the top (`PlayGTA3.exe`) or an installer under
//!    `Redistributables/` is never the game, however large; neither is a wrapper beside it —
//!    an installer, an anti-cheat setup or a publisher's own launcher (Poppy Playtime ships a
//!    chapter launcher this way: `PlaytimeLauncher/PlaytimeLauncher/Binaries/Win64/…`, itself a
//!    full Unreal pairing) — never chosen whatever its size or evidence. Godot: `<stem>.pck`
//!    beside the program.
//! 6. **Steam's hint may name a bootstrapper rather than the game itself** (rule 1 only fires
//!    when the hint passes a rule directly). When it does not, and is not itself a wrapper, its
//!    file stem still favours the matching project over an unrelated one of the same depth and
//!    pairing — ahead of size, behind nothing size would have beaten anyway. [`detect_with`]
//!    never trusts the hint by name alone: a project still needs its own engine evidence.
//!
//! Weaker signs, each at Low confidence: a Unity game from before 2017.2 (its player linked into
//! the program, so no `UnityPlayer`, but `<stem>_Data/Managed/Assembly-CSharp.dll`) ranks with
//! the programs that pass, by depth, after them at the same depth. Only when no game is found at
//! all do the rest speak: half an Unreal pairing, a Godot pck embedded in the program. A wrapper
//! is excluded at every tier, not only the full pairing.
//!
//! Everything above is **Pass A**, and [`detect_with`] is Pass A alone. Since 0.5.0 its rules
//! are data — the built-in engine descriptors `src/engines/builtin/{unity,godot,unreal}.toml`
//! (`astra.engine/1`), evaluated by the one evaluator every descriptor goes through — and canary
//! C1 holds them to launcher 0.4.1's code byte for byte. What stays code is here: the walk, the
//! anti-cheat signatures ([`ANTI_CHEAT_SIGNATURES`], one table), the wrapper names, the
//! one-program fallback; and `best()`'s order, in the evaluator. [`detect_with_engines`] adds
//! **Pass B**: engine descriptors ([`crate::EngineSet`]) asked only when Pass A answers
//! `unknown` with no evidence and no hint used, so a descriptor can name a game nothing here
//! recognised and can never change one it did (see [`crate::engines`]).
//!
//! **Anti-cheat** is asked of every game, after both passes, whatever named it (merged plan
//! §2.4): the folder signatures, the signed catalogue's markers an [`crate::EngineSet`] carries,
//! and a kernel driver installed from the game's folder ([`crate::anti_cheat_services`]). Every
//! code the launcher emits is [`ANTI_CHEAT_CODES`]; planning refuses on each
//! ([`crate::plan_install_in`]).
//!
//! Neither pass follows a link or waits on a FIFO; 0.4.1 did both in a few reads, a declared
//! divergence (`tests/corpus/divergence.txt`).

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engines::{self, EngineCode, EngineSet, NamedEngine};
use crate::error::{LauncherError, Result, codes};

const MAX_DEPTH: usize = 4;
pub(crate) const MAX_ENTRIES: usize = 5000;

/// The engines this launcher recognises by its own code (Pass A). Their codes are reserved: no
/// engine descriptor may claim one ([`crate::engines`]). Where a [`Detection`] reports an engine
/// it is an [`EngineCode`], which also carries engines learned from descriptors.
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
    /// Every variant, `Unknown` last.
    pub const ALL: [Engine; 5] = [
        Engine::UnityMono,
        Engine::UnityIl2cpp,
        Engine::Unreal,
        Engine::Godot,
        Engine::Unknown,
    ];

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

/// How [`Detection::exe`] was told apart from the other programs beside it, when the rule that
/// found it had to choose: an Unreal project with no `-Win64-Shipping.exe`, whose
/// `<Project>/Binaries/Win64/` may hold more than one program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExePick {
    /// The program Steam launches (the hint given to [`detect_with`]) is one of them.
    LaunchConfig,
    /// It is the only program there.
    Only,
    /// It is the largest of several (the first by name of equal sizes).
    Largest,
}

impl ExePick {
    pub fn code(self) -> &'static str {
        match self {
            ExePick::LaunchConfig => "launch-config",
            ExePick::Only => "only",
            ExePick::Largest => "largest",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detection {
    /// The engine's code: one of [`Engine`]'s (`"unity-mono"`, … `"unknown"`, spelled exactly as
    /// before 0.5.0), or a code an engine descriptor declares (Pass B only).
    pub engine: EngineCode,
    pub confidence: Confidence,
    /// The game's program, relative to the folder detected.
    pub exe: Option<PathBuf>,
    pub binary: Binary,
    /// The anti-cheat seen, each code once: [`ANTI_CHEAT_CODES`] but `valve-anti-cheat` (a
    /// plan's refusal only). First the folder signatures' ([`ANTI_CHEAT_SIGNATURES`], in walk
    /// order); on a Pass B answer, then its descriptor's own markers' codes and
    /// `anti-cheat-unknown`; then, for every game, `listed-anti-cheat` (a catalogue marker,
    /// [`EngineSet::with_anti_cheat_markers`]) and `kernel-anti-cheat` (a kernel driver whose
    /// image is in this folder, [`crate::anti_cheat_services`]).
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
    /// How the program was chosen among several it could be, beside [`Detection::evidence`]:
    /// `None` when the rule names one program by itself (Unity's `<stem>_Data`, Unreal's
    /// `-Win64-Shipping.exe`, Godot's `.pck`). For support: it says which one and WHY.
    #[serde(default)]
    pub exe_pick: Option<ExePick>,
    /// The engine's version, read by the engine descriptor that named the game (Pass B only;
    /// Unity keeps [`Detection::unity_version`]). Absent from the JSON when `None`, so every
    /// Pass A detection serialises exactly as before 0.5.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `engine-ambiguous`: two or more descriptors of the same standing name this game with
    /// different codes, and none is preselected — [`Detection::engine`] stays `unknown` and
    /// these are the codes, sorted. Empty otherwise, and then absent from the JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engine_candidates: Vec<EngineCode>,
    /// The SHA-256 (hex, of its canonical JSON) of the engine descriptor that named the game:
    /// Pass B only, `None` for a built-in rule. An install pins it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor_sha256: Option<String>,
}

impl Detection {
    /// `engine-ambiguous`: see [`Detection::engine_candidates`].
    pub fn is_ambiguous(&self) -> bool {
        !self.engine_candidates.is_empty()
    }

    /// Whether Pass B may speak for this result: Pass A said `unknown`, with no evidence and no
    /// launch hint used. Everything else Pass A decided is final. THE narrow-only gate: Pass B
    /// asks it, and so must anything else that would let data name a game's engine (a
    /// moderator's `games[]` row: merged plan §2.1) — never a copy of it.
    pub fn open_to_descriptors(&self) -> bool {
        self.engine.is_unknown() && self.evidence.is_none() && !self.hint_used
    }
}

/// The `(major, minor)` of a Unity version string: `6000.3.1f1` → `(6000, 3)`.
pub fn unity_version_parts(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

pub(crate) struct Entry {
    /// Components below the root, original case.
    pub(crate) rel: Vec<String>,
    pub(crate) is_dir: bool,
}

impl Entry {
    pub(crate) fn name(&self) -> &str {
        self.rel.last().map(String::as_str).unwrap_or("")
    }
}

/// Whether `child`, a folder sitting exactly at the walk's depth bound, ends with one of the
/// paths a built-in descriptor still lists there (`walk.list_at_limit`), ignoring ASCII case.
fn listed_at_limit(child: &[String], at_limit: &[&[String]]) -> bool {
    child.len() == MAX_DEPTH
        && at_limit.iter().any(|p| {
            p.len() <= child.len()
                && child[child.len() - p.len()..]
                    .iter()
                    .zip(p.iter())
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
}

/// What a walk saw: its entries, and whether it stopped at its cap with more left to list.
pub(crate) struct Walk {
    pub(crate) entries: Vec<Entry>,
    pub(crate) capped: bool,
}

/// Breadth first, sorted, bounded, links skipped. One exception to the depth bound: a folder
/// landing exactly on it that a built-in descriptor names in `walk.list_at_limit`
/// ([`listed_at_limit`]) is still listed — never recursed past that single extra hop.
fn walk(root: &Path, at_limit: &[&[String]]) -> Result<Walk> {
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
                return Ok(Walk {
                    entries: out,
                    capped: true,
                });
            }
            let mut child = rel.clone();
            child.push(name);
            if is_dir && (child.len() < MAX_DEPTH || listed_at_limit(&child, at_limit)) {
                queue.push_back((dir.join(child.last().unwrap()), child.clone()));
            }
            out.push(Entry { rel: child, is_dir });
        }
    }
    Ok(Walk {
        entries: out,
        capped: false,
    })
}

/// The walk every detection and the plan's anti-cheat check read: bounded by the built-in
/// descriptors' `walk.list_at_limit`.
fn builtin_walk(root: &Path) -> Result<Walk> {
    walk(root, &engines::builtin().list_at_limit())
}

/// What one anti-cheat signature matches: a name in the walk, compared ignoring ASCII case. Every
/// name here is spelled in lower case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureName {
    /// A folder of exactly this name.
    Folder(&'static str),
    /// A file of exactly this name.
    File(&'static str),
    /// A file whose name starts with the first and ends with the second.
    FileAround(&'static str, &'static str),
}

impl SignatureName {
    /// Whether the walk's entry `lower` (its name, lower-cased) is a folder (`is_dir`) this
    /// names.
    fn matches(self, lower: &str, is_dir: bool) -> bool {
        match self {
            SignatureName::Folder(f) => is_dir && lower == f,
            SignatureName::File(f) => !is_dir && lower == f,
            SignatureName::FileAround(start, end) => {
                !is_dir && lower.starts_with(start) && lower.ends_with(end)
            }
        }
    }
}

/// One anti-cheat the walk can see in a game folder, by the names it ships under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AntiCheatSignature {
    /// The code [`Detection::anti_cheat`] carries, and an `anti-cheat` refusal's `kind`.
    pub code: &'static str,
    /// The names it is seen by; any one is enough.
    pub names: &'static [SignatureName],
}

/// The anti-cheat signatures (merged plan §2.4): the ONE table of the anti-cheat this launcher
/// sees in a game folder by name, never by which game it is. The first five are launcher 0.4.1's
/// markers (main 73bf11e, `detect.rs` :303-332), unchanged: canary C1 holds them to 0.4.1's.
/// The other four are marketplace plan A§4.5's extended ones, each name beside its public source;
/// no real install of them was on hand (docs/ENGINE-DESCRIPTORS-DEBT.md row 23), and each is a
/// declared divergence from 0.4.1 (`tests/corpus/divergence.txt`, `extended-anti-cheat-<code>`).
///
/// Sources cited: SteamDB's file detection rules, built from Steam depot file lists
/// (github.com/SteamDatabase/FileDetectionRuleSets, `rules.ini` `[AntiCheat]`, commit 243cf74);
/// BinaryObjectScanner's Denuvo checks (github.com/SabreTools/BinaryObjectScanner,
/// `BinaryObjectScanner/Protection/Denuvo.cs`, commit 0c23b23), taken from Doom Eternal's Steam
/// depot 782332, and the DRML entry built on it (github.com/TheRogueArchivist/DRML,
/// `Entries/Denuvo_Anti-Cheat/Denuvo_Anti-Cheat.md`, commit 4498cc6).
pub const ANTI_CHEAT_SIGNATURES: &[AntiCheatSignature] = &[
    AntiCheatSignature {
        code: "easy-anti-cheat",
        names: &[
            SignatureName::Folder("easyanticheat"),
            SignatureName::File("easyanticheat_eos_setup.exe"),
            SignatureName::File("start_protected_game.exe"),
        ],
    },
    AntiCheatSignature {
        code: "battleye",
        names: &[
            SignatureName::Folder("battleye"),
            SignatureName::FileAround("beservice", ".exe"),
        ],
    },
    AntiCheatSignature {
        code: "gameguard",
        names: &[SignatureName::Folder("gameguard")],
    },
    AntiCheatSignature {
        code: "xigncode",
        names: &[SignatureName::Folder("xigncode")],
    },
    // HoYoverse's protection.
    AntiCheatSignature {
        code: "hoyo-protect",
        names: &[
            SignatureName::File("mhypbase.dll"),
            SignatureName::File("hoyokprotect.sys"),
        ],
    },
    // EA Javelin (EA AntiCheat). SteamDB `EA_AntiCheat = EAAntiCheat\.Installer\.exe$`; and
    // Steam's own launch entries on a real appinfo.vdf (this desk's, 2026-10-31) start
    // `EAAntiCheat.GameServiceLauncher.exe` from the game's folder. Both: `EAAntiCheat.*.exe`.
    AntiCheatSignature {
        code: "ea-javelin",
        names: &[SignatureName::FileAround("eaanticheat.", ".exe")],
    },
    // ACE, Tencent's Anti-Cheat Expert. SteamDB `AntiCheatExpert[] = AntiCheatExpert/` and
    // `AntiCheatExpert[] = AceAntibotClient/`: two folders.
    AntiCheatSignature {
        code: "anti-cheat-expert",
        names: &[
            SignatureName::Folder("anticheatexpert"),
            SignatureName::Folder("aceantibotclient"),
        ],
    },
    // EQU8. SteamDB `EQU8 = equ8_conf\.json$`.
    AntiCheatSignature {
        code: "equ8",
        names: &[SignatureName::File("equ8_conf.json")],
    },
    // Denuvo Anti-Cheat (Irdeto). BinaryObjectScanner's path checks, from Doom Eternal's depot:
    // `denuvo-anti-cheat.sys`, `denuvo-anti-cheat-update-service.exe`,
    // `denuvo-anti-cheat-runtime.dll`, `denuvo-anti-cheat-update-service-launcher.dll`,
    // `denuvo-anti-cheat-crash-report.exe`, `denuvo-anti-cheat-installer.zip` (all
    // `denuvo-anti-cheat*`), and `Denuvo Anti-Cheat Installer.exe`, "seen in the folder of the
    // main game executable".
    AntiCheatSignature {
        code: "denuvo-anti-cheat",
        names: &[
            SignatureName::FileAround("denuvo-anti-cheat", ""),
            SignatureName::File("denuvo anti-cheat installer.exe"),
        ],
    },
];

const SIGNATURES: usize = ANTI_CHEAT_SIGNATURES.len();

/// [`ANTI_CHEAT_CODES`], built from the tables that spell each code once.
const ANTI_CHEAT_CODE_LIST: [&str; SIGNATURES + 4] = {
    let mut out = [""; SIGNATURES + 4];
    let mut i = 0;
    while i < SIGNATURES {
        out[i] = ANTI_CHEAT_SIGNATURES[i].code;
        i += 1;
    }
    out[SIGNATURES] = codes::LISTED_ANTI_CHEAT;
    out[SIGNATURES + 1] = codes::KERNEL_ANTI_CHEAT;
    out[SIGNATURES + 2] = codes::ANTI_CHEAT_UNKNOWN;
    out[SIGNATURES + 3] = crate::plan::VALVE_ANTI_CHEAT;
    out
};

/// Every anti-cheat code the launcher itself emits (K16): in [`Detection::anti_cheat`] and as an
/// `anti-cheat` refusal's `kind`. The signatures' ([`ANTI_CHEAT_SIGNATURES`]), in their order;
/// then `listed-anti-cheat` (a marker of the signed catalogue's `anticheat[]`, given to
/// [`EngineSet::with_anti_cheat_markers`]); `kernel-anti-cheat` (a kernel driver installed from
/// the game's folder: [`crate::anti_cheat_services`], Windows); `anti-cheat-unknown` (a Pass B
/// answer the walk cannot vouch for; planning refuses a descriptor's code on such a claim); and
/// `valve-anti-cheat` (Steam's VAC flag,
/// a plan's refusal only, never in a [`Detection`]). An `anti-cheat` refusal may also carry the
/// manifest's own `[target] anti_cheat`, and a Pass B answer the codes its descriptor's own
/// markers declare: those are the data's words, not the launcher's.
pub const ANTI_CHEAT_CODES: &[&str] = &ANTI_CHEAT_CODE_LIST;

/// The signatures' codes in the walk's entries, each once, in walk order: Pass A's anti-cheat.
pub(crate) fn anti_cheat_of(entries: &[Entry]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for e in entries {
        let n = e.name().to_ascii_lowercase();
        let kind = ANTI_CHEAT_SIGNATURES
            .iter()
            .find(|s| s.names.iter().any(|name| name.matches(&n, e.is_dir)))
            .map(|s| s.code);
        if let Some(k) = kind
            && !found.iter().any(|f| f == k)
        {
            found.push(k.to_owned());
        }
    }
    found
}

/// The anti-cheat signals every detection and every plan adds after Pass A (and Pass B), for
/// EVERY game, whatever named it: the catalogue's markers in `engines` over the walk
/// (`listed-anti-cheat`), and the machine's services tied to `game_dir` (`kernel-anti-cheat`,
/// and `listed-anti-cheat` for a service a marker names; Windows only). Nothing when `engines`
/// holds no marker and no service is tied to the folder.
fn anti_cheat_beyond_the_walk(
    game_dir: &Path,
    entries: &[Entry],
    engines: &EngineSet,
) -> Vec<String> {
    let mut out = Vec::new();
    if engines.markers().listed_in(entries) {
        out.push(codes::LISTED_ANTI_CHEAT.to_owned());
    }
    for code in crate::anti_cheat_services::of_game(
        game_dir,
        crate::anti_cheat_services::ServiceTree::System,
        engines,
    ) {
        if !out.contains(&code) {
            out.push(code);
        }
    }
    out
}

/// `more` added to `to`, each once, after what it holds.
fn add_codes(to: &mut Vec<String>, more: Vec<String>) {
    for c in more {
        if !to.contains(&c) {
            to.push(c);
        }
    }
}

/// The anti-cheat a game folder carries, as planning asks: the signatures over the built-in walk,
/// then what [`anti_cheat_beyond_the_walk`] adds with `engines`.
pub(crate) fn anti_cheat_scan(game_dir: &Path, engines: &EngineSet) -> Result<Vec<String>> {
    let walked = builtin_walk(game_dir)?;
    let mut found = anti_cheat_of(&walked.entries);
    add_codes(
        &mut found,
        anti_cheat_beyond_the_walk(game_dir, &walked.entries, engines),
    );
    Ok(found)
}

/// Whether `exe` (components below `game_dir`) is a program the built-in walk lists, or Steam's
/// `hint`, that Pass B's tests take ([`engines::is_walked_program`]): what a community
/// integration may plan on when no descriptor named it.
pub(crate) fn is_walked_program(
    game_dir: &Path,
    exe: &[String],
    hint: Option<&str>,
) -> Result<bool> {
    let walked = builtin_walk(game_dir)?;
    let hint = hint.and_then(|h| {
        let mut h = h.trim();
        while let Some(rest) = h.strip_prefix("./").or_else(|| h.strip_prefix(".\\")) {
            h = rest;
        }
        crate::paths::relative_components(h)
    });
    Ok(engines::is_walked_program(
        game_dir,
        &walked.entries,
        exe,
        hint.as_deref(),
    ))
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

pub(crate) fn abs(root: &Path, rel: &[String]) -> PathBuf {
    let mut p = root.to_path_buf();
    p.extend(rel);
    p
}

/// `name` in `listing`, of the kind asked for: the exact spelling first, else ignoring ASCII
/// case (a game built on Windows may not match its own spelling on a Linux disk).
pub(crate) fn find_in<'a>(listing: &'a Listing, name: &str, dir: bool) -> Option<&'a str> {
    let mut it = listing.iter().filter(|(_, d)| *d == dir);
    it.clone()
        .find(|(n, _)| n == name)
        .or_else(|| it.find(|(n, _)| n.eq_ignore_ascii_case(name)))
        .map(|(n, _)| n.as_str())
}

/// One folder's listing, read from disk. `None` when it cannot be read.
pub(crate) fn listing_of(dir: &Path) -> Option<Vec<(String, bool)>> {
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
pub(crate) fn child(dir: &Path, name: &str, want_dir: bool) -> Option<String> {
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
pub(crate) fn program_stem(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _)) if is_program_name(name) => stem,
        _ => name,
    }
}

/// An installer, an anti-cheat bootstrapper, a crash reporter, or a publisher's own launcher
/// (Poppy Playtime's chapter launcher, itself a full Unreal pairing — GTA III's
/// `Rockstar-Games-Launcher.exe`): never the game, whatever evidence sits beside it, and never
/// what a Steam launch hint is followed to. Matched loosely (a substring, lower-cased) because
/// these ship under many exact names; a real game only collides with one by choosing the same
/// word for itself, the trade-off this whole rule already accepts (see the module doc).
pub(crate) fn is_wrapper_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("launcher")
        || lower.contains("rockstar")
        || lower.contains("prereq")
        || lower.contains("vcredist")
        || lower.contains("vc_redist")
        || lower.contains("dxsetup")
        || lower.contains("crashreportclient")
        || lower.contains("crashhandler")
        || lower.starts_with("easyanticheat")
        || lower.starts_with("beservice")
        || lower == "start_protected_game.exe"
}

/// The bytes below `p`, by a bounded walk that never follows a link. A tie-break, so a walk cut
/// short by its bounds still answers.
pub(crate) fn evidence_size(p: &Path) -> u64 {
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

/// The file stem of a launch hint's last path component (`WindowsNoEditor\Poppy_Playtime.exe` →
/// `Poppy_Playtime`), for matching it against a project's own name when the hint itself is a
/// bootstrapper rather than evidence.
pub(crate) fn hint_stem(hint: &str) -> &str {
    let name = hint.rsplit(['/', '\\']).next().unwrap_or(hint);
    program_stem(name)
}

/// Detects the engine of a game folder. Read-only. The same as [`detect_with`] without a hint.
pub fn detect(game_dir: &Path) -> Result<Detection> {
    detect_with(game_dir, None)
}

/// Detects the engine of a game folder, given the program Steam launches for it when known
/// (`launch_hint`, relative to `game_dir`, from [`crate::steam_appinfo::launch_executable`]).
/// Read-only.
///
/// A hint inside the folder that passes a strong built-in rule wins outright and
/// [`Detection::hint_used`] says so. Any other hint (missing, outside the folder, a launcher)
/// is ignored, and `hint_used` is `false`.
///
/// This is Pass A alone: exactly [`detect_with_engines`] with an empty [`EngineSet`] (so a kernel
/// anti-cheat driver installed from this folder still shows, [`crate::anti_cheat_services`]).
pub fn detect_with(game_dir: &Path, launch_hint: Option<&str>) -> Result<Detection> {
    detect_with_engines(game_dir, launch_hint, &EngineSet::empty())
}

/// Pass A (the built-in descriptors), then — only when it answers `unknown` with no evidence and
/// no hint used — Pass B: the engine descriptors in `engines`, evaluated against the same folder
/// within counted budgets (see [`crate::engines`]). Narrow-only by construction: any other Pass A
/// result is returned untouched, whatever `engines` holds.
///
/// Pass B may name the game with a descriptor's code (and then fills
/// [`Detection::descriptor_sha256`], perhaps [`Detection::version`]), or find that descriptors of
/// one standing disagree ([`Detection::engine_candidates`], `engine-ambiguous`), or say nothing,
/// in which case this is exactly [`detect_with`]'s answer.
///
/// Then, whichever pass answered and whatever it said, the anti-cheat signals beyond the folder
/// signatures join [`Detection::anti_cheat`]: `engines`' catalogue markers
/// ([`EngineSet::with_anti_cheat_markers`], `listed-anti-cheat`) over the same walk, and a kernel
/// driver installed from this folder ([`crate::anti_cheat_services`], `kernel-anti-cheat`). They
/// only add: with no marker and no such service this is exactly the passes' answer.
pub fn detect_with_engines(
    game_dir: &Path,
    launch_hint: Option<&str>,
    engines: &EngineSet,
) -> Result<Detection> {
    let (walked, found) = pass_a_of(game_dir, launch_hint)?;
    let mut found = if engines.is_empty() || !found.open_to_descriptors() {
        found
    } else {
        engines::pass_b(game_dir, launch_hint, &walked, engines, found)
    };
    add_codes(
        &mut found.anti_cheat,
        anti_cheat_beyond_the_walk(game_dir, &walked.entries, engines),
    );
    Ok(found)
}

/// Pass A, as [`detect_with_engines`] makes it: the built-ins' walk of `game_dir`, and their
/// answer (the one-program fallback when no rule names a game). Planning's engine check asks
/// this, with the same hint, before it lets a descriptor name a program.
pub(crate) fn pass_a_of(game_dir: &Path, launch_hint: Option<&str>) -> Result<(Walk, Detection)> {
    let walked = builtin_walk(game_dir)?;
    let found = engines::pass_a(game_dir, launch_hint, &walked.entries, engines::builtin())
        .unwrap_or_else(|| one_program(&walked.entries));
    Ok((walked, found))
}

/// No built-in rule named a game: `unknown`, with the program named when there is exactly one
/// obvious one — a top-level program, never a wrapper.
fn one_program(entries: &[Entry]) -> Detection {
    let main: Vec<&Entry> = entries
        .iter()
        .filter(|e| !e.is_dir && e.rel.len() == 1 && is_program_name(e.name()))
        .filter(|e| !is_wrapper_name(e.name()))
        .collect();
    let (exe, binary) = match main.as_slice() {
        [one] => (Some(rel_path(&one.rel)), binary_of(one.name())),
        _ => (None, Binary::Unknown),
    };
    Detection {
        engine: Engine::Unknown.into(),
        confidence: Confidence::Low,
        exe,
        binary,
        anti_cheat: anti_cheat_of(entries),
        evidence: None,
        hint_used: false,
        unity_version: None,
        exe_pick: None,
        version: None,
        engine_candidates: Vec::new(),
        descriptor_sha256: None,
    }
}

pub(crate) fn ext_lower(name: &str) -> String {
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

pub(crate) fn rel_path(rel: &[String]) -> PathBuf {
    rel.iter().collect()
}

/// The engine of the game whose program is `exe_rel`, for planning's engine check: by the
/// built-in rules asked about that one program, read from a fresh listing of its own folder,
/// never through a link — another program in the folder being a game says nothing about this
/// one — and only when none matches, by `engines`: where the game's Pass A answer (with Steam's
/// `launch_hint`, as the daemon detects) is open to descriptors, the descriptor Pass B would
/// choose among the matches of this program ([`engines::engine_of_program`]). `unknown` when
/// nothing matches.
pub(crate) fn engine_of_exe_with(
    engines: &EngineSet,
    game_dir: &Path,
    exe_rel: &[String],
    launch_hint: Option<&str>,
) -> NamedEngine {
    engines::engine_of_program(game_dir, exe_rel, launch_hint, engines::builtin(), engines)
}
