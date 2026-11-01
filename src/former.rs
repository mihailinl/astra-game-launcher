// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Files the Unreal runner 0.2.x (astra-reshade v0.2.0 … v0.2.11) put beside a game's program,
//! recognised as Astra's own when no ledger names them (a lost profile, a deleted ledger, a
//! launcher before 0.4.0 that took them for the player's). Every one of them is also, byte for
//! byte, an upstream release a player may install themselves, so the rules are exact and narrow:
//!
//! 1. **UE4SS's proxy**: `dwmapi.dll` with RE-UE4SS 3.0.1-1164's digest, ONLY when neither
//!    `UE4SS.dll` nor `ue4ss/UE4SS.dll` is beside it. That is what 0.2.0–0.2.3 left (UE4SS itself
//!    was in the profile), the state in which every plain Steam start fails with "Failed to load
//!    UE4SS.dll". With UE4SS beside it, it is a working UE4SS: the player's, for all we can tell.
//! 2. **ReShade**: `dxgi.dll` with ReShade 6.8.0 Addon's digest, ONLY when an `astra.addon64`
//!    beside it has the digest of one we shipped. Then that `dxgi.dll`, that `astra.addon64` and
//!    the `astra-reshade/` folder beside them are one set: Astra's — except
//!    `astra-reshade/AstraPreset.ini`, where our add-on points ReShade's `PresetPath` and ReShade
//!    saves the player's tuning: theirs, as `ReShade.ini` is, so it stays and keeps the folder.
//!    No other name is ever ReShade here, and a ReShade beside no add-on of ours, or beside one we
//!    never shipped, is the player's.
//!
//! A file the current ledger names is never classified here: the ledger governs its own files.
//! What follows: an install takes such a file's place without backing it up (`add`) and removes
//! the others (`InstallPlan::former_files`); an uninstall never "restores" one from a backup, and
//! removes those beside the exe its ledger does not name. Every removal hashes again immediately
//! before it and removes only what is exactly what was judged; a file larger than
//! [`MAX_HASHED_BYTES`] is never one of these.
//!
//! The digests' named source is `tests/fixtures/astra-reshade-unreal/`: every release's own
//! `astra-gi.toml`, verbatim from its GitHub release asset (`tools/fetch-astra-reshade-manifests.sh`).
//! A unit test checks each constant against those files.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::find_any_case;

/// RE-UE4SS 3.0.1-1164-g5e627997's own `dwmapi.dll` proxy, which every astra-reshade 0.2.x
/// release placed beside the game's exe.
pub(crate) const UE4SS_PROXY_SHA256: &str =
    "3f11fad958a959dd7ab78ab0b42be90e33f8ef8e152fbdc3c8e4278cb590e6cc";
/// ReShade 6.8.0 with add-on support (`ReShade64.dll` of `ReShade_Setup_6.8.0_Addon.exe`), the
/// only ReShade astra-reshade 0.2.x shipped, as `dxgi.dll`.
pub(crate) const RESHADE_SHA256: &str =
    "0cee63f9c9f13f3ac909c5b4903f4dbb4b719a7ab3b4f13b0deaf83c814b94f7";
/// Our ReShade add-on `astra.addon64`: one per release, v0.2.0 … v0.2.11 in order.
pub(crate) const ADDON_SHA256: &[&str] = &[
    "3ba2e7a2021163f213849836464d5840c4661960d0d0e9917cb0eddbeda01721",
    "b55e510afecce8be1a39070c925ddd57f826a6c2ed2f21886a25050b073c2ef6",
    "c5a11cc2516efd338fa5c1748248d964d58b809f1e849e5c0e96805a5f13dfc4",
    "b619af228c2d20036cc2eac1e9a48f7081b0cd26a57f9c9e97aec6a3b58a1f88",
    "5a1945aba8a21445fd01292db76565fc29e2484cd9125a8dde08d3184dffd548",
    "5a6ba0f3ddd041269f7215c94df7fd582c09e8825f7289c32a323615a30672d2",
    "bd6f9120f7ac079a9463be48779ae1df8611ad0ad2e47246d32714ec07f1cf0f",
    "0f5d137c18deb1ce51de05ca09392d885f91b0bf1799476611e8f173924a6a7a",
    "1209474b14eff4a74da45d5aa51e7484186aebed02692711d50b595fede6678e",
    "4b9658c910e269d4ebefc8c235c5fea67b9a4a861fb95b9d66037a0729ea3599",
    "996a43853953021a6327745eb4a1efca1f7694c67f0907335137c13385adf0f8",
    "17e5d055bb65a183343ee0c6d30095ed91f528e3bbc156f05189c057af3f3ef2",
];

const PROXY: &str = "dwmapi.dll";
const RESHADE: &str = "dxgi.dll";
const ADDON: &str = "astra.addon64";
/// The effect's folder 0.2.x placed beside the exe (its shaders; ReShade's preset went there too).
pub(crate) const SHADERS: &str = "astra-reshade";
/// ReShade's preset in that folder (astra-reshade `config_rules.h`: our add-on sets `PresetPath`
/// to it), which ReShade rewrites with the player's tuning: never removed with the folder.
const PRESET: &str = "AstraPreset.ini";
/// More than any of these files: a larger file is never hashed here, and never one of them.
pub(crate) const MAX_HASHED_BYTES: u64 = 64 * 1024 * 1024;
/// More entries than the shaders' folder ever held: a larger one is not ours to remove.
const MAX_TREE_ENTRIES: usize = 4096;

/// Which former file of Astra's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FormerKind {
    /// UE4SS's `dwmapi.dll` proxy, with no UE4SS beside it (rule 1).
    Ue4ssProxy,
    /// ReShade 6.8.0 as `dxgi.dll`, beside an add-on of ours (rule 2).
    Reshade,
    /// Our `astra.addon64`, beside that ReShade (rule 2).
    Addon,
    /// The `astra-reshade/` folder beside those two (rule 2), but for the player's
    /// `AstraPreset.ini` in it.
    Shaders,
}

#[cfg(test)]
thread_local! {
    /// Digests a unit test stands in for the shipped files, whose bytes are not committed:
    /// `[ue4ss proxy, reshade, add-on]`.
    pub(crate) static STAND_INS: std::cell::RefCell<Option<[String; 3]>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether a unit test stands `sha256` in for shipped file `which` (0, 1, 2 as in `STAND_INS`).
fn stand_in(which: usize, sha256: &str) -> bool {
    #[cfg(test)]
    if STAND_INS.with(|d| d.borrow().as_ref().is_some_and(|t| t[which] == sha256)) {
        return true;
    }
    let _ = (which, sha256);
    false
}

fn is_ue4ss_proxy(sha256: &str) -> bool {
    sha256 == UE4SS_PROXY_SHA256 || stand_in(0, sha256)
}

fn is_reshade(sha256: &str) -> bool {
    sha256 == RESHADE_SHA256 || stand_in(1, sha256)
}

fn is_addon(sha256: &str) -> bool {
    ADDON_SHA256.contains(&sha256) || stand_in(2, sha256)
}

/// The digest of the regular file at `path` (never through a link) when it is no larger than
/// [`MAX_HASHED_BYTES`]; `None` otherwise: then it is not one of these files.
pub(crate) fn capped_sha256(path: &Path) -> Option<String> {
    let m = fs::symlink_metadata(path).ok()?;
    if !m.is_file() || m.len() > MAX_HASHED_BYTES {
        return None;
    }
    crate::digest::sha256_file(path).ok()
}

/// The entry named `name` (any letter case) directly in `dir`, with its spelling on disk.
fn entry(dir: &Path, name: &str) -> Option<(String, PathBuf)> {
    let p = find_any_case(dir, &[name])?;
    let n = p.file_name()?.to_str()?.to_owned();
    Some((n, p))
}

/// Whether UE4SS itself is beside a proxy in `dir`: `UE4SS.dll` or `ue4ss/UE4SS.dll`.
fn ue4ss_beside(dir: &Path) -> bool {
    find_any_case(dir, &["UE4SS.dll"]).is_some()
        || find_any_case(dir, &["ue4ss", "UE4SS.dll"]).is_some()
}

/// `(spelling, digest)` of the file named `name` in `dir`, when its digest passes `is`.
fn shipped(dir: &Path, name: &str, is: fn(&str) -> bool) -> Option<(String, String)> {
    let (n, p) = entry(dir, name)?;
    let sha256 = capped_sha256(&p)?;
    is(&sha256).then_some((n, sha256))
}

/// What the file `name` in the folder `dir` is, given its digest `sha256` (of the file there now,
/// or of a backup that would be put back there): rule 1 or rule 2 exactly, else nothing.
pub(crate) fn former_kind(dir: &Path, name: &str, sha256: &str) -> Option<FormerKind> {
    if name.eq_ignore_ascii_case(PROXY) {
        (is_ue4ss_proxy(sha256) && !ue4ss_beside(dir)).then_some(FormerKind::Ue4ssProxy)
    } else if name.eq_ignore_ascii_case(RESHADE) {
        (is_reshade(sha256) && shipped(dir, ADDON, is_addon).is_some())
            .then_some(FormerKind::Reshade)
    } else if name.eq_ignore_ascii_case(ADDON) {
        (is_addon(sha256) && shipped(dir, RESHADE, is_reshade).is_some())
            .then_some(FormerKind::Addon)
    } else {
        None
    }
}

/// One former file (or the shaders' folder) directly in the exe's folder, with the digest it was
/// judged by (the folder's: [`tree_digest`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Former {
    /// Its spelling on disk.
    pub name: String,
    pub sha256: String,
    pub kind: FormerKind,
}

/// Every former file of Astra's directly in `dir`, the exe's folder, sorted by name: rule 1's
/// proxy, and rule 2's whole set.
pub(crate) fn former_set(dir: &Path) -> Vec<Former> {
    let mut out = Vec::new();
    if let Some((name, sha256)) = shipped(dir, PROXY, is_ue4ss_proxy)
        && !ue4ss_beside(dir)
    {
        out.push(Former {
            name,
            sha256,
            kind: FormerKind::Ue4ssProxy,
        });
    }
    if let (Some((rname, rsha)), Some((aname, asha))) = (
        shipped(dir, RESHADE, is_reshade),
        shipped(dir, ADDON, is_addon),
    ) {
        out.push(Former {
            name: rname,
            sha256: rsha,
            kind: FormerKind::Reshade,
        });
        out.push(Former {
            name: aname,
            sha256: asha,
            kind: FormerKind::Addon,
        });
        if let Some((name, path)) = entry(dir, SHADERS)
            && let Some(sha256) = tree_digest(&path)
        {
            out.push(Former {
                name,
                sha256,
                kind: FormerKind::Shaders,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Whether `e`, an entry directly in the shaders' folder, is the player's preset (a regular file
/// named [`PRESET`] in any letter case).
fn is_preset(e: &fs::DirEntry) -> bool {
    e.file_name()
        .to_str()
        .is_some_and(|n| n.eq_ignore_ascii_case(PRESET))
        && e.file_type().is_ok_and(|t| t.is_file())
}

/// A digest of the shaders' folder at `dir` and everything below it (each entry's path, each
/// file's digest), the player's preset left out: what a removal takes. `None` when it is not a
/// folder (a link is not), holds anything but files and folders (a link, a FIFO), is larger than
/// the bounds, or holds nothing but the preset (nothing to remove).
pub(crate) fn tree_digest(dir: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    if !fs::symlink_metadata(dir).ok()?.is_dir() {
        return None;
    }
    let mut lines: Vec<String> = Vec::new();
    let (mut entries, mut bytes) = (0usize, 0u64);
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((path, rel)) = stack.pop() {
        for e in fs::read_dir(&path).ok()? {
            let e = e.ok()?;
            if rel.is_empty() && is_preset(&e) {
                continue;
            }
            entries += 1;
            if entries > MAX_TREE_ENTRIES {
                return None;
            }
            let child = format!("{rel}{}", e.file_name().to_str()?);
            let m = fs::symlink_metadata(e.path()).ok()?;
            if m.is_dir() {
                lines.push(format!("{child}/"));
                stack.push((e.path(), format!("{child}/")));
            } else if m.is_file() {
                bytes += m.len();
                if bytes > MAX_HASHED_BYTES {
                    return None;
                }
                let sha256 = crate::digest::sha256_file(&e.path()).ok()?;
                lines.push(format!("{sha256}  {child}"));
            } else {
                return None;
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    lines.sort();
    let mut h = Sha256::new();
    for l in &lines {
        h.update(l.as_bytes());
        h.update(b"\n");
    }
    Some(hex::encode(h.finalize()))
}

/// What became of a former file when it was to be removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Removal {
    Removed,
    /// Nothing there any more.
    Absent,
    /// Something else is there now: left alone.
    Changed,
}

/// Removes `name` (a file, or the shaders' folder) directly in `dir`, hashing it again right
/// before: only while it is exactly what was judged (`sha256`). Never through a link.
pub(crate) fn remove_exact(
    dir: &Path,
    name: &str,
    kind: FormerKind,
    sha256: &str,
) -> std::io::Result<Removal> {
    let path = dir.join(name);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Removal::Absent),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    if kind == FormerKind::Shaders {
        if tree_digest(&path).as_deref() != Some(sha256) {
            return Ok(Removal::Changed);
        }
        // The player's preset stays, and with it the folder; everything else of ours goes.
        let entries: Vec<fs::DirEntry> = fs::read_dir(&path)?.collect::<std::io::Result<_>>()?;
        if entries.iter().any(is_preset) {
            for e in entries.iter().filter(|e| !is_preset(e)) {
                if e.file_type()?.is_dir() {
                    fs::remove_dir_all(e.path())?;
                } else {
                    fs::remove_file(e.path())?;
                }
            }
        } else {
            fs::remove_dir_all(&path)?;
        }
    } else {
        if capped_sha256(&path).as_deref() != Some(sha256) {
            return Ok(Removal::Changed);
        }
        fs::remove_file(&path)?;
    }
    Ok(Removal::Removed)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::digest::sha256_hex;
    use crate::{
        Accept, Ctx, GameAction, GameTarget, GiPackage, Platform, codes, install, installed,
        plan_install, uninstall,
    };

    /// Test-only bytes standing in for the shipped files.
    const UE4SS_PROXY: &[u8] = b"MZ stand-in: RE-UE4SS 3.0.1-1164 dwmapi.dll";
    const RESHADE_BYTES: &[u8] = b"MZ stand-in: ReShade 6.8.0 Addon";
    const ADDON_BYTES: &[u8] = b"MZ stand-in: astra.addon64 0.2.11";
    const LOADER: &[u8] = b"MZ astra_loader: an inert dwmapi forwarder";
    const ACCEPT: Accept = Accept {
        vac_unchecked: true,
    };

    fn stand_ins() {
        STAND_INS.with(|d| {
            *d.borrow_mut() = Some([
                sha256_hex(UE4SS_PROXY),
                sha256_hex(RESHADE_BYTES),
                sha256_hex(ADDON_BYTES),
            ])
        });
    }

    fn no_stand_ins() {
        STAND_INS.with(|d| *d.borrow_mut() = None);
    }

    struct World(PathBuf);
    impl Drop for World {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl World {
        fn new(name: &str) -> World {
            let p = std::env::temp_dir().join(format!("agl-former-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(p.join("game")).unwrap();
            fs::write(p.join("game/Game.exe"), b"MZ game").unwrap();
            World(fs::canonicalize(p).unwrap())
        }
        fn game(&self) -> PathBuf {
            self.0.join("game")
        }
        fn put(&self, rel: &str, bytes: &[u8]) {
            let p = self.game().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, bytes).unwrap();
        }
        fn profile(&self) -> PathBuf {
            self.0.join("profile")
        }
        fn target(&self) -> GameTarget {
            GameTarget {
                dir: self.game(),
                exe: PathBuf::from("Game.exe"),
                steam_appid: None,
                platform: Platform::Windows,
                steam_appinfo: Some(self.0.join("no-appinfo.vdf")),
            }
        }
        fn names(&self) -> Vec<String> {
            let mut v: Vec<String> = fs::read_dir(self.game())
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        }
        fn install(&self, pkg: &GiPackage) -> crate::InstallPlan {
            let plan = plan_install(pkg, &self.target(), &self.profile()).unwrap();
            install(pkg, &plan, &self.target(), ACCEPT, &Ctx::silent()).unwrap();
            plan
        }
        fn uninstall(&self) -> crate::UninstallReport {
            uninstall(&self.profile(), &self.target()).unwrap()
        }
    }

    /// A package whose game files are `game_files` (each beside the exe).
    fn package(game_files: &[(&str, &[u8])]) -> GiPackage {
        let mut manifest = String::from(
            "schema = 1\n[integration]\nid = \"astra.test\"\nname = \"Test\"\nversion = \"1.0.0\"\n\
             [target]\nexe = \"*\"\nplatforms = [\"windows-x64\"]\nanti_cheat = \"none\"\n\
             [[files]]\nfrom = \"p/x.txt\"\nto = \"${profile}/p/x.txt\"\n",
        );
        for (name, bytes) in game_files {
            manifest.push_str(&format!(
                "[[game_files]]\nfrom = \"{name}\"\nto = \"${{game}}/{name}\"\nsha256 = \"{}\"\n",
                sha256_hex(bytes)
            ));
        }
        manifest.push_str("[launch]\nargs = [\"--astra\"]\n");
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file("astra-gi.toml", opts).unwrap();
        w.write_all(manifest.as_bytes()).unwrap();
        w.start_file("p/x.txt", opts).unwrap();
        w.write_all(b"x").unwrap();
        for (name, bytes) in game_files {
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        let bytes = w.finish().unwrap().into_inner();
        GiPackage::open(&bytes, &sha256_hex(&bytes)).unwrap()
    }

    /// Today's Unreal runner: our own `dwmapi.dll`.
    fn unreal() -> GiPackage {
        package(&[("dwmapi.dll", LOADER)])
    }

    /// A runner that places nothing these rules know.
    fn other() -> GiPackage {
        package(&[("winhttp.dll", b"MZ some proxy")])
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                let rel = p.strip_prefix(dir).unwrap().display().to_string();
                if p.is_dir() {
                    out.push((format!("{rel}/"), Vec::new()));
                    stack.push(p);
                } else {
                    out.push((rel, fs::read(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    // ---- the constants, bound to their named source ----

    /// Every Unreal runner release astra-reshade published (v0.2.0 … v0.2.11), its own
    /// `astra-gi.toml` verbatim from the GitHub release asset `astra-gi.zip`
    /// (`tools/fetch-astra-reshade-manifests.sh`): what each one pinned for `dwmapi.dll`,
    /// `dxgi.dll` and `astra.addon64` is what the constants must be.
    #[test]
    fn the_digests_are_what_every_0_2_release_pinned() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/astra-reshade-unreal");
        let mut releases: Vec<((u32, u32, u32), toml::Table)> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| {
                let text = fs::read_to_string(e.path()).unwrap();
                let t: toml::Table = toml::from_str(&text).unwrap();
                let v = t["integration"]["version"].as_str().unwrap().to_owned();
                let n: Vec<u32> = v.split('.').map(|p| p.parse().unwrap()).collect();
                ((n[0], n[1], n[2]), t)
            })
            .collect();
        releases.sort_by_key(|(v, _)| *v);
        assert_eq!(
            releases.iter().map(|(v, _)| *v).collect::<Vec<_>>(),
            (0..=11).map(|p| (0, 2, p)).collect::<Vec<_>>(),
            "v0.2.0 … v0.2.11, each once"
        );
        let pinned = |t: &toml::Table, name: &str| -> String {
            t["game_files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|g| g["from"].as_str() == Some(name))
                .and_then(|g| g["sha256"].as_str())
                .unwrap()
                .to_owned()
        };
        let mut addons = Vec::new();
        for (v, t) in &releases {
            assert_eq!(pinned(t, "dwmapi.dll"), UE4SS_PROXY_SHA256, "{v:?}");
            assert_eq!(pinned(t, "dxgi.dll"), RESHADE_SHA256, "{v:?}");
            addons.push(pinned(t, "astra.addon64"));
        }
        assert_eq!(addons, ADDON_SHA256, "one add-on per release, in order");
    }

    // ---- rule 1: UE4SS's proxy ----

    #[test]
    fn ue4ss_s_proxy_with_ue4ss_beside_it_is_the_players() {
        stand_ins();
        for beside in ["ue4ss/UE4SS.dll", "UE4SS.dll"] {
            let w = World::new(&format!("ue4ss-beside-{}", beside.len()));
            w.put("dwmapi.dll", UE4SS_PROXY);
            w.put(beside, b"MZ UE4SS");
            assert!(former_set(&w.game()).is_empty());
            // A working loader of the player's, by a name loaders hijack (step 10a): never
            // replaced. (A launcher before 0.5.0 replaced it with a backup.)
            let e = plan_install(&unreal(), &w.target(), &w.profile()).unwrap_err();
            assert_eq!(
                (e.code, e.param("file")),
                (codes::FOREIGN_LOADER, Some("dwmapi.dll"))
            );
            // A package shipping the very same bytes reuses them.
            let same = package(&[("dwmapi.dll", UE4SS_PROXY)]);
            let plan = plan_install(&same, &w.target(), &w.profile()).unwrap();
            assert_eq!(plan.game_files[0].action, GameAction::ReuseKnownProxy);
        }
    }

    #[test]
    fn ue4ss_s_proxy_alone_is_astras_and_goes() {
        stand_ins();
        // Today's runner takes its place: no backup, nothing put back.
        let w = World::new("ue4ss-alone");
        w.put("dwmapi.dll", UE4SS_PROXY);
        let plan = w.install(&unreal());
        let g = &plan.game_files[0];
        assert_eq!(
            (g.action, g.existing_sha256.clone()),
            (GameAction::Add, Some(sha256_hex(UE4SS_PROXY)))
        );
        assert!(
            installed(&w.profile()).unwrap().unwrap().game_files[0]
                .backup
                .is_none()
        );
        let r = w.uninstall();
        assert_eq!(
            (r.removed, r.restored),
            (vec!["dwmapi.dll".to_owned()], vec![])
        );
        assert_eq!(w.names(), ["Game.exe"]);

        // A runner that does not place it: listed and removed by the install.
        let w = World::new("ue4ss-alone-other");
        w.put("dwmapi.dll", UE4SS_PROXY);
        let plan = w.install(&other());
        let former: Vec<(&str, FormerKind)> = plan
            .former_files
            .iter()
            .map(|f| (f.rel_path.as_str(), f.kind))
            .collect();
        assert_eq!(former, [("dwmapi.dll", FormerKind::Ue4ssProxy)]);
        assert!(plan.warnings.contains(&"former-astra-files".to_owned()));
        assert_eq!(w.names(), ["Game.exe", "winhttp.dll"]);
    }

    /// What a launcher before 0.4.0 did over a lost ledger (and one before 0.5.0 over any UE4SS):
    /// `theirs` at `dwmapi.dll`, replaced by ours and backed up. This launcher never makes that
    /// `replace` (step 10a), so it is laid as that launcher left it: installed over nothing, then
    /// the ledger and the backup made what a `replace` writes.
    fn replaced_by_an_older_launcher(w: &World, theirs: &[u8]) {
        let _ = fs::remove_file(w.game().join("dwmapi.dll"));
        assert_eq!(w.install(&unreal()).game_files[0].action, GameAction::Add);
        let profile = fs::canonicalize(w.profile()).unwrap();
        let mut l = crate::ledger::read(&profile).unwrap().unwrap();
        let rel = format!("{}/game/dwmapi.dll", crate::paths::BACKUP_DIR);
        let backup = profile.join(&rel);
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, theirs).unwrap();
        l.game_files[0].action = GameAction::Replace;
        l.game_files[0].backup = Some(crate::LedgerBackup {
            rel,
            sha256: sha256_hex(theirs),
        });
        crate::ledger::write(&profile, &l).unwrap();
    }

    #[test]
    fn a_backup_of_ue4ss_s_proxy_is_put_back_only_with_ue4ss_beside_it() {
        for ue4ss in [false, true] {
            no_stand_ins();
            let w = World::new(&format!("backup-{ue4ss}"));
            if ue4ss {
                w.put("ue4ss/UE4SS.dll", b"MZ UE4SS");
            }
            replaced_by_an_older_launcher(&w, UE4SS_PROXY);
            stand_ins();
            // An update plans for what an uninstall would leave: with UE4SS beside it the
            // player's loader, kept in the backup and said so (10a).
            let update = plan_install(&unreal(), &w.target(), &w.profile()).unwrap();
            let want = if ue4ss {
                (GameAction::Replace, Some(sha256_hex(UE4SS_PROXY)))
            } else {
                (GameAction::Add, None)
            };
            assert_eq!(
                (
                    update.game_files[0].action,
                    update.game_files[0].existing_sha256.clone()
                ),
                want
            );
            assert_eq!(
                update
                    .warnings
                    .contains(&codes::FOREIGN_LOADER_IN_BACKUP.to_owned()),
                ue4ss
            );
            let r = w.uninstall();
            if ue4ss {
                assert_eq!(r.restored, ["dwmapi.dll"]);
                assert!(r.not_restored.is_empty());
                assert_eq!(fs::read(w.game().join("dwmapi.dll")).unwrap(), UE4SS_PROXY);
            } else {
                assert_eq!(
                    (r.not_restored.clone(), r.removed.clone()),
                    (vec!["dwmapi.dll".to_owned()], vec!["dwmapi.dll".to_owned()])
                );
                assert!(r.restored.is_empty());
                assert_eq!(w.names(), ["Game.exe"]);
            }
        }
        // The same through the update itself.
        no_stand_ins();
        let w = World::new("backup-update");
        replaced_by_an_older_launcher(&w, UE4SS_PROXY);
        stand_ins();
        w.install(&unreal());
        let ledger = installed(&w.profile()).unwrap().unwrap();
        assert_eq!(
            (
                ledger.game_files[0].action,
                ledger.game_files[0].backup.is_none()
            ),
            (GameAction::Add, true)
        );
        w.uninstall();
        assert_eq!(w.names(), ["Game.exe"]);
    }

    // ---- rule 2: ReShade, the add-on, the shaders ----

    #[test]
    fn our_reshade_add_on_and_shaders_are_one_set_and_go_together() {
        stand_ins();
        let w = World::new("set");
        w.put("dxgi.dll", RESHADE_BYTES);
        w.put("astra.addon64", ADDON_BYTES);
        w.put("astra-reshade/Shaders/AstraPassthrough.fx", b"// ours");
        w.put("ReShade.ini", b"[GENERAL]\n");
        let plan = w.install(&other());
        let former: Vec<(&str, FormerKind)> = plan
            .former_files
            .iter()
            .map(|f| (f.rel_path.as_str(), f.kind))
            .collect();
        assert_eq!(
            former,
            [
                ("astra-reshade", FormerKind::Shaders),
                ("astra.addon64", FormerKind::Addon),
                ("dxgi.dll", FormerKind::Reshade)
            ]
        );
        assert_eq!(w.names(), ["Game.exe", "ReShade.ini", "winhttp.dll"]);
        w.uninstall();
        assert_eq!(w.names(), ["Game.exe", "ReShade.ini"]);
    }

    #[test]
    fn the_preset_reshade_saved_the_players_tuning_into_stays_with_its_folder() {
        stand_ins();
        let w = World::new("preset");
        w.put("dxgi.dll", RESHADE_BYTES);
        w.put("astra.addon64", ADDON_BYTES);
        w.put("astra-reshade/Shaders/AstraPassthrough.fx", b"// ours");
        w.put("astra-reshade/astrapreset.INI", b"Techniques=Astra\n");
        let plan = plan_install(&other(), &w.target(), &w.profile()).unwrap();
        assert!(
            plan.former_files
                .iter()
                .any(|f| (f.rel_path.as_str(), f.kind) == ("astra-reshade", FormerKind::Shaders))
        );
        // The player tunes ReShade once more between the plan and the install: still theirs,
        // and no reason to call the plan stale.
        w.put(
            "astra-reshade/astrapreset.INI",
            b"Techniques=Astra\nSharpen=1\n",
        );
        install(&other(), &plan, &w.target(), ACCEPT, &Ctx::silent()).unwrap();
        assert_eq!(w.names(), ["Game.exe", "astra-reshade", "winhttp.dll"]);
        let left: Vec<String> = fs::read_dir(w.game().join("astra-reshade"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["astrapreset.INI"]);
        assert_eq!(
            fs::read(w.game().join("astra-reshade/astrapreset.INI")).unwrap(),
            b"Techniques=Astra\nSharpen=1\n"
        );
        // And the uninstall leaves it, folder and all.
        assert!(former_set(&w.game()).is_empty());
        let r = w.uninstall();
        assert!(r.former_removed.is_empty());
        assert_eq!(w.names(), ["Game.exe", "astra-reshade"]);
    }

    #[test]
    fn a_reshade_beside_no_add_on_of_ours_is_never_touched() {
        stand_ins();
        for addon in [None, Some(&b"MZ an add-on we never shipped"[..])] {
            let w = World::new(&format!("players-reshade-{}", addon.is_some()));
            w.put("dxgi.dll", RESHADE_BYTES);
            if let Some(a) = addon {
                w.put("astra.addon64", a);
            }
            assert!(former_set(&w.game()).is_empty());
            // A package with a dxgi.dll of its own: another loader, as before.
            let e = plan_install(
                &package(&[("dxgi.dll", b"MZ our dxgi")]),
                &w.target(),
                &w.profile(),
            )
            .unwrap_err();
            assert_eq!(
                (e.code, e.param("file")),
                (codes::FOREIGN_LOADER, Some("dxgi.dll"))
            );
            let before = snapshot(&w.game());
            w.install(&other());
            w.uninstall();
            assert_eq!(snapshot(&w.game()), before);
        }
    }

    #[test]
    fn a_dxgi_the_player_updated_beside_our_add_on_is_not_former() {
        stand_ins();
        let w = World::new("updated-dxgi");
        w.put("dxgi.dll", b"MZ ReShade 6.9.0, theirs now");
        w.put("astra.addon64", ADDON_BYTES);
        assert!(former_set(&w.game()).is_empty());
        let before = snapshot(&w.game());
        w.install(&other());
        w.uninstall();
        assert_eq!(snapshot(&w.game()), before);
    }

    #[test]
    fn a_players_reshade_under_another_name_is_untouched_by_an_update() {
        stand_ins();
        let w = World::new("other-name");
        w.put("d3d11.dll", RESHADE_BYTES);
        w.put("astra.addon64", ADDON_BYTES);
        let before = snapshot(&w.game());
        w.install(&other());
        // The update, over a live ledger.
        let update = w.install(&other());
        assert!(update.former_files.is_empty());
        // A package with a d3d11.dll of its own: theirs is another loader, never Astra's.
        let e = plan_install(
            &package(&[("d3d11.dll", b"MZ our d3d11")]),
            &w.target(),
            &w.profile(),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.param("file")),
            (codes::FOREIGN_LOADER, Some("d3d11.dll"))
        );
        let r = w.uninstall();
        assert!(r.former_removed.is_empty());
        assert_eq!(snapshot(&w.game()), before);
    }

    #[test]
    fn a_live_installs_own_files_are_never_former() {
        stand_ins();
        // A 0.2.x-like runner whose ledger is alive: its files are the ledger's.
        let w = World::new("live");
        let v02 = package(&[("dxgi.dll", RESHADE_BYTES), ("astra.addon64", ADDON_BYTES)]);
        let plan = w.install(&v02);
        assert!(
            plan.game_files
                .iter()
                .all(|g| (g.action, g.existing_sha256.as_deref()) == (GameAction::Add, None))
        );
        let update = w.install(&v02);
        assert!(update.former_files.is_empty());
        assert!(
            update
                .game_files
                .iter()
                .all(|g| g.existing_sha256.is_none())
        );
        let r = w.uninstall();
        assert!(r.former_removed.is_empty());
        assert_eq!(r.removed.len(), 2, "removed by the ledger, as its own");
        assert_eq!(w.names(), ["Game.exe"]);

        // A ledger a launcher before 0.4.0 wrote over the very same bytes: `reuse-known-proxy`.
        // They are that ledger's files: an update keeps them so, and its uninstall leaves them.
        no_stand_ins();
        let w = World::new("live-reused");
        w.put("dxgi.dll", RESHADE_BYTES);
        w.put("astra.addon64", ADDON_BYTES);
        w.install(&v02);
        stand_ins();
        let update = plan_install(&v02, &w.target(), &w.profile()).unwrap();
        assert!(update.former_files.is_empty());
        assert!(
            update
                .game_files
                .iter()
                .all(|g| g.action == GameAction::ReuseKnownProxy),
            "{:?}",
            update.game_files
        );
        let r = w.uninstall();
        assert!(r.former_removed.is_empty());
        assert_eq!(w.names(), ["Game.exe", "astra.addon64", "dxgi.dll"]);
    }

    // ---- removal: exactly what was judged, nothing larger than the bound ----

    #[test]
    fn a_former_file_is_removed_only_while_it_is_exactly_the_one_judged() {
        stand_ins();
        let w = World::new("exact");
        w.put("dxgi.dll", RESHADE_BYTES);
        w.put("astra.addon64", ADDON_BYTES);
        w.put("astra-reshade/Shaders/x.fx", b"// ours");
        let set = former_set(&w.game());
        assert_eq!(set.len(), 3);
        // Each changes after it was judged.
        w.put("dxgi.dll", b"MZ something else now");
        w.put("astra-reshade/Shaders/x.fx", b"// edited");
        w.put("astra.addon64", b"MZ another add-on");
        for f in &set {
            assert_eq!(
                remove_exact(&w.game(), &f.name, f.kind, &f.sha256).unwrap(),
                Removal::Changed,
                "{f:?}"
            );
        }
        assert_eq!(
            w.names(),
            ["Game.exe", "astra-reshade", "astra.addon64", "dxgi.dll"]
        );
        fs::remove_file(w.game().join("dxgi.dll")).unwrap();
        assert_eq!(
            remove_exact(&w.game(), "dxgi.dll", FormerKind::Reshade, &set[2].sha256).unwrap(),
            Removal::Absent
        );
    }

    #[test]
    fn nothing_larger_than_the_hash_bound_is_ever_former() {
        // A file one byte over the bound, whose digest a unit test then stands in for UE4SS's.
        let w = World::new("large");
        let p = w.game().join("dwmapi.dll");
        let f = fs::File::create(&p).unwrap();
        f.set_len(MAX_HASHED_BYTES + 1).unwrap();
        drop(f);
        let large = crate::digest::sha256_file(&p).unwrap();
        STAND_INS.with(|d| *d.borrow_mut() = Some([large, "-".into(), "-".into()]));
        assert!(former_set(&w.game()).is_empty());
        assert_eq!(capped_sha256(&p), None);
    }
}
