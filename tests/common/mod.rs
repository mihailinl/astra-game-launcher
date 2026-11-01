// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Test fixtures, all built at run time: no binary fixture is committed.

#![allow(dead_code)]

pub mod identity;

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use astra_game_launcher::{
    Ctx, GameTarget, GiPackage, InstallPlan, Platform, install, plan_install,
};
use sha2::{Digest, Sha256};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

pub fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A folder removed when dropped.
pub struct Tmp(pub PathBuf);

static N: AtomicU64 = AtomicU64::new(0);

impl Tmp {
    pub fn new(name: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!(
            "agl-test-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(fs::canonicalize(&p).unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

pub enum E<'a> {
    File(&'a str, &'a [u8]),
    Dir(&'a str),
    Symlink(&'a str, &'a str),
}

pub fn zip(entries: &[E]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for e in entries {
        match e {
            E::File(n, b) => {
                w.start_file(*n, opts).unwrap();
                w.write_all(b).unwrap();
            }
            E::Dir(n) => w.add_directory(*n, opts).unwrap(),
            E::Symlink(n, t) => w.add_symlink(*n, *t, opts).unwrap(),
        }
    }
    w.finish().unwrap().into_inner()
}

pub const WINHTTP: &[u8] = b"MZ fake doorstop proxy";
pub const DOORSTOP_INI: &[u8] = b"[General]\nenabled = false\n";
pub const PRELOADER: &[u8] = b"fake BepInEx.Preloader.dll";
pub const CORE_DLL: &[u8] = b"fake BepInEx.dll";

/// A runner manifest shaped like astra-bepinex's v0.5.0, with `extra` appended.
pub fn runner_manifest(extra: &str) -> String {
    format!(
        r#"schema = 1

[integration]
id = "test.unity"
name = "Test runner"
version = "1.0.0"
authors = ["tester"]
license = "MIT"
some_future_metadata = "ignored"

[target]
engine = "unity-mono"
exe = "*"
platforms = ["windows-x64", "linux-proton", "linux-x64"]
anti_cheat = "none"

[[files]]
from = "BepInEx/core/**"
to = "${{profile}}/BepInEx/core/"

[[game_files]]
from = "winhttp.dll"
to = "${{game}}/winhttp.dll"
sha256 = "{}"

[[game_files]]
from = "doorstop_config.ini"
to = "${{game}}/doorstop_config.ini"

[launch]
args = ["--doorstop-enabled", "true", "--doorstop-target-assembly", "${{profile}}/BepInEx/core/BepInEx.Preloader.dll"]
proton_dll_overrides = {{ winhttp = "native,builtin" }}

[[config_writes]]
file = "${{profile}}/BepInEx/config/astra.cfg"
section = "Connection"
set = {{ Port = "${{bridge_port}}", Token = "${{bridge_token}}" }}
{extra}"#,
        sha(WINHTTP)
    )
}

/// The runner zip around `manifest`.
pub fn runner_zip(manifest: &str) -> Vec<u8> {
    zip(&[
        E::File("astra-gi.toml", manifest.as_bytes()),
        E::File("winhttp.dll", WINHTTP),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::Dir("BepInEx/"),
        E::Dir("BepInEx/core/"),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
        E::File("licenses/LICENSE.txt", b"MIT"),
    ])
}

pub const X64: u16 = 0x8664;
pub const DOORSTOP_V4: &[&str] = &["--doorstop-enabled", "--doorstop-target-assembly"];

/// A PE image for `machine` carrying `options` as NUL-terminated UTF-16LE strings, the way a
/// Windows Doorstop build carries the command-line options it understands; `tag` tells two
/// builds apart.
pub fn pe_with_options(machine: u16, options: &[&str], tag: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 0x80];
    b[..2].copy_from_slice(b"MZ");
    b[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    b[0x40..0x44].copy_from_slice(b"PE\0\0");
    b[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
    for o in options {
        b.extend(o.encode_utf16().flat_map(u16::to_le_bytes));
        b.extend([0, 0]);
    }
    b.extend_from_slice(tag);
    b
}

/// The runner, shipping a proxy shaped like a real Doorstop 4 build (`ours`).
pub fn doorstop_runner(ours: &[u8]) -> GiPackage {
    let m = runner_manifest("").replace(&sha(WINHTTP), &sha(ours));
    open(&zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", ours),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
    ]))
    .unwrap()
}

pub fn open(zip: &[u8]) -> Result<GiPackage, astra_game_launcher::LauncherError> {
    GiPackage::open(zip, &sha(zip))
}

pub fn runner() -> GiPackage {
    open(&runner_zip(&runner_manifest(""))).unwrap()
}

/// A Unity Mono game for Windows: `<name>.exe`, `UnityPlayer.dll`, `<name>_Data/Managed/Assembly-CSharp.dll`.
pub fn unity_game(dir: &Path, name: &str) {
    write(&dir.join(format!("{name}.exe")), b"MZ game");
    write(&dir.join("UnityPlayer.dll"), b"MZ player");
    write(
        &dir.join(format!("{name}_Data/Managed/Assembly-CSharp.dll")),
        b"MZ csharp",
    );
}

pub fn target(dir: &Path, exe: &str, appid: Option<u32>, platform: Platform) -> GameTarget {
    GameTarget {
        dir: dir.to_path_buf(),
        exe: PathBuf::from(exe),
        steam_appid: appid,
        platform,
        // Never this machine's Steam: a missing file, so the VAC check reads "unchecked".
        steam_appinfo: Some(dir.join("no-appinfo.vdf")),
    }
}

/// Every file below `dir` with its bytes, for byte-exact before/after comparisons.
pub fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let ft = e.file_type().unwrap();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if ft.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                walk(root, &p, out);
            } else {
                out.insert(rel, fs::read(&p).unwrap_or_default());
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

/// A copy of the tree at `from` as `to`: folders, files with their bytes, and links as links
/// (their target text unchanged, never followed).
pub fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let ft = e.file_type().unwrap();
        if ft.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(&src).unwrap(), &dst).unwrap();
        } else if ft.is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

/// Every test that is not about VAC installs a plan whose VAC flag no appinfo could read.
pub const ACCEPT: astra_game_launcher::Accept = astra_game_launcher::Accept {
    vac_unchecked: true,
};

pub fn plan_and_install(pkg: &GiPackage, game: &GameTarget, profile: &Path) -> InstallPlan {
    let plan = plan_install(pkg, game, profile).unwrap();
    install(pkg, &plan, game, ACCEPT, &Ctx::silent()).unwrap();
    plan
}

pub fn vars() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("bridge_port".to_owned(), "5555".to_owned()),
        ("bridge_token".to_owned(), "s3cret".to_owned()),
    ])
}

/// A `user.reg` shaped like a Proton prefix's.
pub const USER_REG: &str = "WINE REGISTRY Version 2\n;; All keys relative to REGISTRY\\\\User\\\\S-1-5-21-0-0-1000-1000\n\n#arch=win64\n\n[Control Panel\\\\Desktop] 1784681205\n#time=1dcf9d1a3c4e000\n\"FontSmoothing\"=\"2\"\n\n[Software\\\\Wine\\\\AppDefaults\\\\Other.exe\\\\DllOverrides] 1784681205\n#time=1dcf9d1a3c4e000\n\"dxgi\"=\"native\"\n\n[Software\\\\Wine\\\\DllOverrides] 1784681205\n#time=1dcf9d1a3c4e000\n\"winhttp\"=\"builtin\"\n";

/// One `String` entry of a PE version resource (`ProductVersion`, `ProductName`, …), laid out as a
/// linker writes it: header, UTF-16LE key and NUL, padding to 32 bits, UTF-16LE value and NUL.
pub fn version_entry(key: &str, value: &str) -> Vec<u8> {
    let wide = |s: &str| -> Vec<u8> {
        let mut v: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
        v.extend([0, 0]);
        v
    };
    let (key, val) = (wide(key), wide(value));
    let head = (6 + key.len()).div_ceil(4) * 4;
    let mut out = Vec::new();
    out.extend(((head + val.len()) as u16).to_le_bytes());
    out.extend(((val.len() / 2) as u16).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(&key);
    out.resize(head, 0);
    out.extend(&val);
    out.resize(out.len().div_ceil(4) * 4, 0);
    out
}

/// A PE image whose version resource carries `strings`.
pub fn pe_with_version(strings: &[(&str, &str)]) -> Vec<u8> {
    let mut b = pe_with_options(X64, &[], b"assembly\0\0\0\0");
    for (k, v) in strings {
        b.extend(version_entry(k, v));
    }
    b
}

/// The chain loader's two files, as a runner zip carries them.
pub const CHAIN_DLL: &[u8] = b"MZ fake Astra.Chain.dll";
pub const CHAIN_HOOKS_DLL: &[u8] = b"MZ fake Astra.Chain.Hooks.dll";

/// `[integration.mods]` as the Mono runner declares it.
pub const MODS_DECL: &str = "[integration.mods]\nloader = \"bepinex5\"\nchain = \"${profile}/chain/Astra.Chain.dll\"\nchain_config = \"${profile}/chain/chain.ini\"\n";

/// The runner shipping `proxy` as its Doorstop, the chain loader in `chain/`, and `decl` (a
/// `[integration.mods]` table, or nothing) after everything else.
pub fn mods_runner(proxy: &[u8], decl: &str) -> GiPackage {
    let files = "\n[[files]]\nfrom = \"chain/**\"\nto = \"${profile}/chain/\"\n";
    let m = runner_manifest(&format!("{files}\n{decl}")).replace(&sha(WINHTTP), &sha(proxy));
    open(&zip(&[
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", proxy),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
        E::File("chain/Astra.Chain.dll", CHAIN_DLL),
        E::File("chain/Astra.Chain.Hooks.dll", CHAIN_HOOKS_DLL),
    ]))
    .unwrap()
}

/// A BepInEx 5 below `root` (a profile, or a game folder): its preloader and a `BepInEx.dll`
/// whose version resource says `version`.
pub fn bepinex5_at(root: &Path, version: &str) {
    write(
        &root.join("BepInEx/core/BepInEx.Preloader.dll"),
        b"MZ preloader",
    );
    write(
        &root.join("BepInEx/core/BepInEx.dll"),
        &pe_with_version(&[("ProductName", "BepInEx"), ("ProductVersion", version)]),
    );
}

/// A BepInEx 6 for IL2CPP below `root`, `BepInEx.Core.dll` saying `version`, with its `dotnet/`.
pub fn bepinex6_il2cpp_at(root: &Path, version: &str) {
    write(
        &root.join("BepInEx/core/BepInEx.Unity.IL2CPP.dll"),
        b"MZ il2cpp preloader",
    );
    write(
        &root.join("BepInEx/core/BepInEx.Preloader.Core.dll"),
        b"MZ core preloader",
    );
    write(
        &root.join("BepInEx/core/BepInEx.Core.dll"),
        &pe_with_version(&[("ProductName", "BepInEx.Core"), ("ProductVersion", version)]),
    );
    write(&root.join("dotnet/coreclr.dll"), b"MZ coreclr");
}

/// A mod of the player's: `BepInEx/plugins/<rel>`.
pub fn plugin_at(root: &Path, rel: &str) {
    write(&root.join("BepInEx/plugins").join(rel), b"MZ a mod");
}

/// A Steam `appinfo.vdf` (format v28, inline keys) holding `apps`: each appid with
/// `appinfo/common/category`'s `category_8` (Valve Anti-Cheat) set when `Some(true)`, only
/// `category_1` when `Some(false)`, and no `category` at all when `None` (Steam does not say).
/// The minimum `valve_anti_cheat` reads; `tests/steam_appinfo.rs` builds every other shape.
pub fn appinfo_vac(apps: &[(u32, Option<bool>)]) -> Vec<u8> {
    fn key(out: &mut Vec<u8>, ty: u8, k: &str) {
        out.push(ty);
        out.extend_from_slice(k.as_bytes());
        out.push(0);
    }
    fn string(out: &mut Vec<u8>, k: &str, v: &str) {
        key(out, 0x01, k);
        out.extend_from_slice(v.as_bytes());
        out.push(0);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x0756_4428u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    for (id, vac) in apps {
        let mut body = Vec::new();
        key(&mut body, 0x00, "appinfo");
        key(&mut body, 0x02, "appid");
        body.extend_from_slice(&(*id as i32).to_le_bytes());
        key(&mut body, 0x00, "common");
        string(&mut body, "name", "A game");
        if let Some(vac) = vac {
            key(&mut body, 0x00, "category");
            string(
                &mut body,
                if *vac { "category_8" } else { "category_1" },
                "1",
            );
            body.push(0x08);
        }
        string(&mut body, "type", "Game");
        body.push(0x08); // common
        body.push(0x08); // appinfo
        body.push(0x08); // the record
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(60 + body.len() as u32).to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&1_700_000_000u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&[0xAB; 20]);
        out.extend_from_slice(&42u32.to_le_bytes());
        out.extend_from_slice(&[0xCD; 20]);
        out.extend_from_slice(&body);
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// An `appinfo.vdf` (v28) of apps that each launch one program: `(appid, executable, os)`, as
/// the entry `config/launch/0` of type `default` for that OS — what `launch_executable` reads
/// as the program Steam starts. No `common/category`: the VAC flag is not known for them.
pub fn appinfo_launch(apps: &[(u32, &str, &str)]) -> Vec<u8> {
    fn key(out: &mut Vec<u8>, ty: u8, k: &str) {
        out.push(ty);
        out.extend_from_slice(k.as_bytes());
        out.push(0);
    }
    fn string(out: &mut Vec<u8>, k: &str, v: &str) {
        key(out, 0x01, k);
        out.extend_from_slice(v.as_bytes());
        out.push(0);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x0756_4428u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    for (id, exe, os) in apps {
        let mut body = Vec::new();
        key(&mut body, 0x00, "appinfo");
        key(&mut body, 0x02, "appid");
        body.extend_from_slice(&(*id as i32).to_le_bytes());
        key(&mut body, 0x00, "common");
        string(&mut body, "name", "A game");
        string(&mut body, "type", "Game");
        body.push(0x08); // common
        key(&mut body, 0x00, "config");
        key(&mut body, 0x00, "launch");
        key(&mut body, 0x00, "0");
        string(&mut body, "executable", exe);
        string(&mut body, "type", "default");
        key(&mut body, 0x00, "config");
        string(&mut body, "oslist", os);
        body.push(0x08); // config
        body.push(0x08); // 0
        body.push(0x08); // launch
        body.push(0x08); // config
        body.push(0x08); // appinfo
        body.push(0x08); // the record
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(60 + body.len() as u32).to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&1_700_000_000u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&[0xAB; 20]);
        out.extend_from_slice(&42u32.to_le_bytes());
        out.extend_from_slice(&[0xCD; 20]);
        out.extend_from_slice(&body);
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}
