// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Test fixtures, all built at run time: no binary fixture is committed.

#![allow(dead_code)]

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

pub fn plan_and_install(pkg: &GiPackage, game: &GameTarget, profile: &Path) -> InstallPlan {
    let plan = plan_install(pkg, game, profile).unwrap();
    install(pkg, &plan, game, &Ctx::silent()).unwrap();
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
