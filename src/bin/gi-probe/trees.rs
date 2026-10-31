// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The synthetic game `compat` installs a release on: the least a Steam game folder holds for
//! launcher 0.3.1 to plan, install and launch that release the way Astra 0.2.7 would.
//!
//! It is chosen by the source's KIND and by what the manifest declares, never by which game it
//! is (no per-game list):
//! - the program is the one Astra 0.2.7 hands the launcher: a game source's own `exe` when the
//!   table names one (0.2.7 installs those on that program, not on what detection finds); else the
//!   manifest's `[target] exe`; else, for a runner (`exe = "*"`), the program a game of its engine
//!   would have;
//! - the folder carries what launcher 0.3.1's detector needs to answer the engine `[target]`
//!   names (a runner's engine, a game source's own when its manifest names one, Unity Mono for a
//!   game source whose program detection finds: `pins.rs` says every such one is a Mono build).
//!   These layouts are 0.3.1's built-in rules (`v0.3.1:src/detect.rs`), the only engines it has;
//!   an engine it does not know (`[target] engine = "source2"`) never gets this far, since 0.3.1
//!   refuses the manifest at parse.
//! - the game is a Steam app: `steamapps/common/<dir>` with an `appmanifest_<appid>.acf` naming
//!   it, so the appid is the folder's own (trusted, as 0.3.1's `trusted_appid` reads it) — the
//!   game source's appid, or [`RUNNER_APPID`] for a runner (Astra 0.2.7 installs only on Steam
//!   games). The platform is Windows.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use launcher_031::{GameTarget, Manifest, Platform};

use crate::v027::pins::{GiSource, SourceTarget};

/// The Steam appid of the game a runner is installed on: any Steam game (Astra 0.2.7's own tests
/// use it for "a game with no integration of its own").
pub const RUNNER_APPID: u32 = 4_513_480;

/// The game's folder below `steamapps/common`.
const INSTALL_DIR: &str = "Game";

/// A laid tree: the game, and the Steam program a launch names (never created: launcher 0.3.1's
/// `prepare_launch` only names it in the command it returns).
pub struct Tree {
    pub game: GameTarget,
    pub steam: PathBuf,
}

/// The program `compat` installs on, `/`-separated below the game's folder.
pub fn exe_for(row: &GiSource, m: &Manifest) -> String {
    if let Some(exe) = row.exe() {
        return exe.to_owned();
    }
    if m.target.exe != "*" {
        return m.target.exe.clone();
    }
    match engine_for(row, m).as_deref() {
        Some("unreal") => "Game/Binaries/Win64/Game-Win64-Shipping.exe".to_owned(),
        _ => "Game.exe".to_owned(),
    }
}

/// The engine the tree is laid for: what `[target]` names, else a runner's own engine, else Unity
/// Mono for a game source whose program detection finds (see the module doc).
pub fn engine_for(row: &GiSource, m: &Manifest) -> Option<String> {
    if let Some(e) = &m.target.engine {
        return Some(e.clone());
    }
    match row.target {
        SourceTarget::Runner { engine } => Some(engine.to_owned()),
        SourceTarget::Game { exe: None, .. } => Some("unity-mono".to_owned()),
        SourceTarget::Game { exe: Some(_), .. } => None,
    }
}

/// The appid of the game `row`'s release is installed on.
pub fn appid_for(row: &GiSource) -> u32 {
    row.appid()
        .and_then(|a| a.parse().ok())
        .unwrap_or(RUNNER_APPID)
}

/// Lays the game for `row`'s release `m` below `root` (which must not exist yet).
pub fn lay(root: &Path, row: &GiSource, m: &Manifest) -> io::Result<Tree> {
    let steamapps = root.join("library").join("steamapps");
    let dir = steamapps.join("common").join(INSTALL_DIR);
    fs::create_dir_all(&dir)?;
    let appid = appid_for(row);
    fs::write(
        steamapps.join(format!("appmanifest_{appid}.acf")),
        format!(
            "\"AppState\"\n{{\n\t\"appid\"\t\t\"{appid}\"\n\t\"name\"\t\t\"gi-probe\"\n\t\"installdir\"\t\t\"{INSTALL_DIR}\"\n}}\n"
        ),
    )?;
    let exe = exe_for(row, m);
    let parts: Vec<&str> = exe.split('/').collect();
    let (name, parent) = parts.split_last().expect("split always yields one part");
    let folder = parent.iter().fold(dir.clone(), |p, c| p.join(c));
    write(&folder.join(name), &pe_x64())?;
    let stem = name.rsplit_once('.').map_or(*name, |(s, _)| s);
    match engine_for(row, m).as_deref() {
        Some("unity-mono") => {
            write(&folder.join("UnityPlayer.dll"), &pe_x64())?;
            write(
                &folder.join(format!("{stem}_Data/Managed/Assembly-CSharp.dll")),
                &pe_x64(),
            )?;
        }
        Some("unity-il2cpp") => {
            write(&folder.join("UnityPlayer.dll"), &pe_x64())?;
            write(&folder.join("GameAssembly.dll"), &pe_x64())?;
            write(
                &folder.join(format!(
                    "{stem}_Data/il2cpp_data/Metadata/global-metadata.dat"
                )),
                b"\xaf\x1b\xb1\xfa",
            )?;
        }
        Some("unreal") => {
            // `<Project>/Binaries/Win64/<exe>` beside `<Project>/Content/Paks/`.
            let project = match parent {
                [p @ .., b, w]
                    if b.eq_ignore_ascii_case("Binaries") && w.eq_ignore_ascii_case("Win64") =>
                {
                    p.iter().fold(dir.clone(), |d, c| d.join(c))
                }
                _ => folder.clone(),
            };
            let leaf = project
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(INSTALL_DIR)
                .to_owned();
            write(
                &project.join(format!("Content/Paks/{leaf}-Windows.pak")),
                b"\xe1\x12\x6f\x5a",
            )?;
        }
        Some("godot") => write(&folder.join(format!("{stem}.pck")), b"GDPC")?,
        _ => {}
    }
    Ok(Tree {
        game: GameTarget {
            dir: dir.clone(),
            exe: PathBuf::from(&exe),
            steam_appid: Some(appid),
            platform: Platform::Windows,
            // Never this machine's Steam first: a file that is not there. (0.3.1 still asks every
            // Steam install it finds, so a VAC flag there would refuse, as it would in 0.2.7.)
            steam_appinfo: Some(root.join("no-appinfo.vdf")),
        },
        steam: root.join("Steam").join("steam.exe"),
    })
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)
}

/// The smallest PE image header an x64 program has: `MZ`, `e_lfanew`, `PE\0\0`, AMD64.
fn pe_x64() -> Vec<u8> {
    let mut b = vec![0u8; 0x80];
    b[..2].copy_from_slice(b"MZ");
    b[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    b[0x40..0x44].copy_from_slice(b"PE\0\0");
    b[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
    b
}
