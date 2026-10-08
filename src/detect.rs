// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Which engine a game folder holds, which binary it ships, and whether it carries anti-cheat.
//! Read-only. The walk is bounded (depth 4, 5000 entries) and never follows a link.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result};

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
    /// Codes: "easy-anti-cheat", "battleye", "gameguard", "xigncode".
    pub anti_cheat: Vec<String>,
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

/// What one `<stem>_Data` folder says about the Unity game beside it.
struct UnityCandidate {
    engine: Engine,
    confidence: Confidence,
    exe: Option<PathBuf>,
    binary: Binary,
    score: u8,
}

fn unity_at(root: &Path, data_rel: &[String]) -> Option<UnityCandidate> {
    let data_name = data_rel.last()?;
    let stem = data_name.strip_suffix("_Data")?;
    if stem.is_empty() {
        return None;
    }
    let parent_rel = &data_rel[..data_rel.len() - 1];
    let mut parent = root.to_path_buf();
    parent.extend(parent_rel);
    let data = parent.join(data_name);

    let managed = data.join("Managed");
    let csharp = is_file(&managed.join("Assembly-CSharp.dll"));
    let il2cpp = is_dir(&data.join("il2cpp_data"));
    let player_dll = is_file(&parent.join("UnityPlayer.dll"));
    let player_so = is_file(&parent.join("UnityPlayer.so"));
    let ga = is_file(&parent.join("GameAssembly.dll")) || is_file(&parent.join("GameAssembly.so"));

    let rel_of = |name: &str| -> PathBuf {
        let mut p = PathBuf::new();
        p.extend(parent_rel);
        p.push(name);
        p
    };
    let win_exe = format!("{stem}.exe");
    let (exe, binary) = if is_file(&parent.join(&win_exe)) {
        (Some(rel_of(&win_exe)), Binary::Windows)
    } else if let Some(n) = [
        format!("{stem}.x86_64"),
        format!("{stem}.x86"),
        stem.to_owned(),
    ]
    .into_iter()
    .find(|n| is_file(&parent.join(n)))
    {
        (Some(rel_of(&n)), Binary::Linux)
    } else if player_so {
        (None, Binary::Linux)
    } else if player_dll {
        (None, Binary::Windows)
    } else {
        (None, Binary::Unknown)
    };

    let player = player_dll || player_so;
    let (engine, confidence, score) = if il2cpp && ga {
        (Engine::UnityIl2cpp, Confidence::High, 4)
    } else if csharp && player {
        (Engine::UnityMono, Confidence::High, 4)
    } else if csharp {
        (Engine::UnityMono, Confidence::Low, 2)
    } else if il2cpp {
        (Engine::UnityIl2cpp, Confidence::Low, 2)
    } else if player && is_dir(&managed) {
        (Engine::UnityMono, Confidence::Low, 1)
    } else {
        return None;
    };
    let score = score + u8::from(exe.is_some());
    Some(UnityCandidate {
        engine,
        confidence,
        exe,
        binary,
        score,
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

/// Detects the engine of a game folder. Read-only.
pub fn detect(game_dir: &Path) -> Result<Detection> {
    let entries = walk(game_dir)?;
    let anti_cheat = anti_cheat_of(&entries);

    // Unity: a `<name>_Data` folder at the top or one level down.
    let best = entries
        .iter()
        .filter(|e| e.is_dir && e.rel.len() <= 2 && e.name().ends_with("_Data"))
        .filter_map(|e| unity_at(game_dir, &e.rel))
        .max_by_key(|c| c.score);
    if let Some(c) = best {
        return Ok(Detection {
            engine: c.engine,
            confidence: c.confidence,
            exe: c.exe,
            binary: c.binary,
            anti_cheat,
        });
    }

    // Unreal: `<Project>-Win64-Shipping.exe` and `<Project>/Content/Paks/*.pak`.
    let shipping = entries.iter().find(|e| {
        !e.is_dir
            && e.name()
                .to_ascii_lowercase()
                .ends_with("-win64-shipping.exe")
    });
    let pak = entries.iter().any(|e| {
        let n = e.rel.len();
        !e.is_dir
            && n >= 3
            && ext_lower(e.name()) == "pak"
            && e.rel[n - 2].eq_ignore_ascii_case("Paks")
            && e.rel[n - 3].eq_ignore_ascii_case("Content")
    });
    if shipping.is_some() || pak {
        return Ok(Detection {
            engine: Engine::Unreal,
            confidence: if shipping.is_some() && pak {
                Confidence::High
            } else {
                Confidence::Low
            },
            exe: shipping.map(|e| rel_path(&e.rel)),
            binary: if shipping.is_some() {
                Binary::Windows
            } else {
                Binary::Unknown
            },
            anti_cheat,
        });
    }

    // Godot: a `.pck` beside a program, or a program with an embedded pck.
    let top_programs: Vec<&Entry> = entries
        .iter()
        .filter(|e| !e.is_dir && e.rel.len() == 1 && is_program_name(e.name()))
        .collect();
    let top_pcks: Vec<&Entry> = entries
        .iter()
        .filter(|e| !e.is_dir && e.rel.len() == 1 && ext_lower(e.name()) == "pck")
        .collect();
    if !top_pcks.is_empty() && !top_programs.is_empty() {
        let same_stem = top_programs.iter().find(|p| {
            let stem = p.name().rsplit_once('.').map(|(s, _)| s).unwrap_or("");
            top_pcks
                .iter()
                .any(|k| k.name().rsplit_once('.').map(|(s, _)| s) == Some(stem))
        });
        let exe = same_stem.copied().unwrap_or(top_programs[0]);
        return Ok(Detection {
            engine: Engine::Godot,
            confidence: if same_stem.is_some() {
                Confidence::High
            } else {
                Confidence::Low
            },
            exe: Some(rel_path(&exe.rel)),
            binary: binary_of(exe.name()),
            anti_cheat,
        });
    }
    if let Some(exe) = top_programs
        .iter()
        .take(8)
        .find(|e| has_embedded_pck(&e.path(game_dir)))
    {
        return Ok(Detection {
            engine: Engine::Godot,
            confidence: Confidence::Low,
            exe: Some(rel_path(&exe.rel)),
            binary: binary_of(exe.name()),
            anti_cheat,
        });
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
    })
}

/// The engine of the game whose program is `exe_rel`: its own `<stem>_Data` decides for Unity;
/// otherwise the folder's detection.
pub(crate) fn engine_of_exe(game_dir: &Path, exe_rel: &[String]) -> Result<Engine> {
    if let Some((exe, parents)) = exe_rel.split_last() {
        let stem = exe.rsplit_once('.').map(|(s, _)| s).unwrap_or(exe);
        let mut data_rel: Vec<String> = parents.to_vec();
        data_rel.push(format!("{stem}_Data"));
        if let Some(c) = unity_at(game_dir, &data_rel) {
            return Ok(c.engine);
        }
    }
    Ok(detect(game_dir)?.engine)
}
