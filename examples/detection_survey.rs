// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Runs detection over every game in this machine's Steam libraries, each with the program
//! Steam launches as its hint, and prints a Markdown table: `docs/detection-survey.md`.
//! Read-only. Prints game folder names and paths relative to them, never a machine's paths.
//!
//! ```text
//! cargo run --example detection_survey [-- <steam-root>]
//! cargo run --example detection_survey -- --appids <appid>...   # what Steam launches, per OS
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use astra_game_launcher::{detect_with, launch_executable, steam_roots};

/// The values of every `"<key>" "<value>"` line in a text VDF.
fn vdf_values(text: &str, key: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let t: Vec<&str> = l.split('"').collect();
            (t.len() >= 5 && t[1].eq_ignore_ascii_case(key)).then(|| t[3].replace("\\\\", "\\"))
        })
        .collect()
}

/// Whether `rel` names a file below `dir`, ignoring ASCII case the way Windows (and Proton) do.
fn exists_ignoring_case(dir: &Path, rel: &str) -> bool {
    let mut at = dir.to_path_buf();
    for part in rel.split('/') {
        let Some(found) = fs::read_dir(&at).into_iter().flatten().flatten().find(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(part))
        }) else {
            return false;
        };
        at = found.path();
    }
    at.is_file()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (roots, appids): (Vec<PathBuf>, Vec<u32>) = match args.first().map(String::as_str) {
        Some("--appids") => (
            steam_roots(),
            args[1..].iter().filter_map(|a| a.parse().ok()).collect(),
        ),
        Some(r) => (vec![PathBuf::from(r)], Vec::new()),
        None => (steam_roots(), Vec::new()),
    };
    let Some(root) = roots
        .iter()
        .find(|r| r.join("appcache/appinfo.vdf").is_file())
    else {
        eprintln!("no Steam root with appcache/appinfo.vdf");
        std::process::exit(1);
    };
    let appinfo = root.join("appcache/appinfo.vdf");
    if !appids.is_empty() {
        println!("| App ID | windows | linux |");
        println!("|---|---|---|");
        for id in appids {
            let cell =
                |os| launch_executable(&appinfo, id, os).map_or("-".into(), |e| format!("`{e}`"));
            println!("| {id} | {} | {} |", cell("windows"), cell("linux"));
        }
        return;
    }

    let mut libraries: Vec<PathBuf> = vec![root.clone()];
    if let Ok(t) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
        for p in vdf_values(&t, "path") {
            let p = PathBuf::from(p);
            let same = |a: &Path, b: &Path| fs::canonicalize(a).ok() == fs::canonicalize(b).ok();
            if !libraries.iter().any(|l| same(l, &p)) {
                libraries.push(p);
            }
        }
    }

    let mut rows: Vec<String> = Vec::new();
    for lib in &libraries {
        let steamapps = lib.join("steamapps");
        // installdir → appid, from the app manifests.
        let mut appids: Vec<(String, u32)> = Vec::new();
        for e in fs::read_dir(&steamapps).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            let Ok(t) = fs::read_to_string(e.path()) else {
                continue;
            };
            if let (Some(id), Some(dir)) = (
                vdf_values(&t, "appid").first().and_then(|s| s.parse().ok()),
                vdf_values(&t, "installdir").into_iter().next(),
            ) {
                appids.push((dir, id));
            }
        }
        let mut games: Vec<PathBuf> = fs::read_dir(steamapps.join("common"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        games.sort();
        for game in games {
            let name = game
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let appid = appids
                .iter()
                .find(|(d, _)| d.eq_ignore_ascii_case(&name))
                .map(|(_, id)| *id);
            // The OS the game runs as, the way a frontend knows it: on Linux, a game with a
            // Proton prefix runs its Windows build. When that OS's program is not on disk (a
            // native game once started under Proton), the other OS's entry is asked.
            let proton =
                appid.is_some_and(|id| steamapps.join("compatdata").join(id.to_string()).is_dir());
            let os_order: &[&str] = if cfg!(windows) {
                &["windows"]
            } else if proton {
                &["windows", "linux"]
            } else {
                &["linux", "windows"]
            };
            let hint = appid.and_then(|id| {
                let asked: Vec<String> = os_order
                    .iter()
                    .filter_map(|os| launch_executable(&appinfo, id, os))
                    .collect();
                asked
                    .iter()
                    .find(|exe| exists_ignoring_case(&game, exe))
                    .or(asked.first())
                    .cloned()
            });
            let cell = |p: Option<&Path>| {
                p.map(|p| format!("`{}`", p.display()))
                    .unwrap_or_else(|| "-".into())
            };
            let row = match detect_with(&game, hint.as_deref()) {
                Ok(d) => format!(
                    "| {name} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    appid.map_or("-".into(), |a| a.to_string()),
                    hint.as_deref().map_or("-".into(), |h| format!("`{h}`")),
                    d.engine.code(),
                    serde_json::to_value(d.confidence)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                    cell(d.exe.as_deref()),
                    cell(d.evidence.as_deref()),
                    if d.hint_used { "yes" } else { "no" },
                    if d.anti_cheat.is_empty() {
                        "-".into()
                    } else {
                        d.anti_cheat.join(", ")
                    },
                ),
                Err(e) => format!("| {name} | error: {} | | | | | | | |", e.code),
            };
            rows.push(row);
        }
    }
    println!(
        "| Game folder | App ID | Steam launches | Engine | Confidence | Exe | Evidence | Hint used | Anti-cheat |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    for r in rows {
        println!("{r}");
    }
}
