// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The mods identity canary (0.5.1 step C): the player's mods are now found through the built-in
//! `mods` data (`src/mods/builtin/*.mods.toml`, engine-descriptor-v1.md §7) instead of launcher
//! 0.5.0's hand-written BepInEx code, and over a seeded corpus of Unity game folders, mod-manager
//! homes and Astra profiles this crate's `discover_mods_in` answers EXACTLY what launcher 0.5.0's
//! (v0.5.0, 6d1851b, a dev-dependency) answers — every [`ModSource`] field, in the same order:
//! manager, profile, root, loader kind, version and preloader, mods, last played, the playable
//! verdict and its parameters — but for the rows declared in [`Row`], each applied to 0.5.0's
//! answer (a source changed, gone, or one 0.5.1 lists where 0.5.0 listed none) and counted, and
//! each with a floor, so a row nothing exercises fails as surely as one that is wrong. Floors per
//! playable verdict, manager and loader kind hold the corpus to covering what 0.5.0 could say.
//! Where a row changes the loader 0.5.1 reads, its verdict is 0.5.0's rule ([`verdict_050`],
//! itself held to every verdict 0.5.0 gives over the corpus) on what 0.5.1 read.
//!
//! The corpus lays the real layouts (astra-companion-5f's loader notes, 2026-11-01): BepInEx 5 in
//! a game folder (`BepInEx/`, `winhttp.dll`, `doorstop_config.ini`, `.doorstop_version`) and in a
//! r2modman, Thunderstore Mod Manager or Gale profile (`<profile>/BepInEx/{core,plugins,patchers,
//! config}`, `LogOutput.log`), plugins as folders and loose dlls, mods switched off as `*.old`;
//! BepInEx 6 IL2CPP with `dotnet/` and `BepInEx/interop/`, at be.788 and others; BepInEx 6 Mono,
//! CoreCLR and the IL2CPP preloader's older name; MelonLoader (`version.dll`, `MelonLoader/`,
//! `Mods/`, `Plugins/`, `UserData/`); Astra's own items, Astra's own BepInEx in the game folder;
//! folders the player picked; every integration verdict 0.5.0 gave; and what players' trees hold
//! that the data reads otherwise: a `BepInEx/` spelled in another letter case, links, a version
//! resource with no `ProductVersion` or with control characters, both BepInEx cores in one
//! profile, more mods than one evaluation reads. Unreal is new in 0.5.1 and has nothing to be
//! identical to: `tests/mods.rs` holds its answers.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use astra_game_launcher as now;
use astra_game_launcher::Platform;
use common::*;
use serde_json::Value;

/// Fixed for good: a new seed is a new corpus.
const SEED: u64 = 0xA57A_0510;
/// The gate's size, and what `cargo test` runs. `AGL_MODS_WORLDS=200` for an inner loop; the
/// floors scale with it.
const WORLDS: u64 = 600;
const WORLDS_ENV: &str = "AGL_MODS_WORLDS";

const GAME: &str = "Some Game";
/// Lethal Company: in Thunderstore's table (r2modman's `LethalCompany`, Gale's `lethal-company`).
const LETHAL: u32 = 1_966_720;
/// Not in the table: found by the install folder's name, or a log naming the program.
const NEW_APPID: u32 = 4_000_510;

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

/// Where 0.5.1 knowingly answers otherwise than 0.5.0, each the built-in data's reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Row {
    /// 0.5.0 counted `BepInEx/patchers`; the data's mods root is `BepInEx/plugins`
    /// (`unity.mods.toml` `root`): fewer mods, and a profile of patchers alone is no source.
    Patchers,
    /// A plugins folder holding no `*.dll` — a pack of textures another mod reads, an empty
    /// folder, or (where no manager's adapter reads its state: a picked folder, the game folder)
    /// one switched off as `*.old` — was no mod to 0.5.0; the data's `dir-per-mod` counts it.
    FolderWithoutDll,
    /// The same, where it is all a source holds: 0.5.0 listed no source there, 0.5.1 lists one
    /// (and, when it is offered, a Play that had no answer asks: `mods-choice-needed`).
    SourceAppears,
    /// A BepInEx 6 IL2CPP that never ran has no `BepInEx/interop/` yet, which the data requires
    /// to recognise it: no source.
    Il2cppNeverRun,
    /// A BepInEx 5 in an IL2CPP game, a BepInEx 6 IL2CPP in a Mono game: the data asks each in
    /// its own flavour only (`codes`), and neither runs in the other. No source.
    AnotherFlavour,
    /// A Unity game whose flavour nothing names (`unknown`): no built-in data applies. No
    /// BepInEx source.
    FlavourUnknown,
    /// The preloader without its `BepInEx.dll` (`BepInEx.Core.dll` for 6) beside it: the data
    /// requires both; 0.5.0 listed it with no version. No source.
    LibraryMissing,
    /// The game folder's own BepInEx beside a program below the folder detection runs on: the
    /// data's `game` source is that folder (§7.3), not the program's. No source.
    ProgramBelowTheFolder,
    /// An IL2CPP integration declaring a build other than 788: the data's `versions =
    /// "=6.0.0-be.788"` does not hold for its own build (a foundation may narrow it, never widen
    /// it, §7.4), so no BepInEx 6 of the player's is offered with it, whatever its build:
    /// `not-supported{reason=outside-versions}` where 0.5.0 offered the declared build and told
    /// any other "need the declared build".
    BuildNot788,
    /// On a file system that tells letter case apart: a `BepInEx/` or `BepInEx/core/` spelled in
    /// another case (`bepinex/`, `BepInEx/Core/`) is not the data's (`stat` is the exact
    /// spelling, §7.4), where 0.5.0 found it ignoring case (and Proton loads it). No source.
    CaseVariant,
    /// Unix: a link is never followed below `{source}` (§7.3). A mod folder or a loose dll that is
    /// a link is no mod (fewer mods, and a source of linked mods alone is none); a `BepInEx/`
    /// that is a link recognises nothing (no source).
    Link,
    /// A `BepInEx.dll` (`BepInEx.Core.dll`) whose `ProductVersion` is missing or empty: the data
    /// reads its `FileVersion` next (`key = ["ProductVersion", "FileVersion"]`), where 0.5.0's
    /// version was unknown. Another version, and the verdict on it.
    VersionFallback,
    /// A version resource holding control characters: shown without them (and `have` too).
    Printable,
    /// In an IL2CPP game, a profile whose `BepInEx/core` holds both BepInEx 5's preloader and
    /// BepInEx 6 IL2CPP's: 0.5.0 read the first it probed (BepInEx 5, never offered there), the
    /// data reads the IL2CPP game's own (BepInEx 6, its version and verdict).
    MixedCores,
    /// More than 1,024 mods in the root (§7.8): 0.5.1 lists none of them and says
    /// `not-supported{reason=over-budget}` (no version, no preloader); 0.5.0 counted them.
    OverBudget,
}

const ROWS: [Row; 15] = [
    Row::Patchers,
    Row::FolderWithoutDll,
    Row::SourceAppears,
    Row::Il2cppNeverRun,
    Row::AnotherFlavour,
    Row::FlavourUnknown,
    Row::LibraryMissing,
    Row::ProgramBelowTheFolder,
    Row::BuildNot788,
    Row::CaseVariant,
    Row::Link,
    Row::VersionFallback,
    Row::Printable,
    Row::MixedCores,
    Row::OverBudget,
];

/// The loader 0.5.1 reads in a source the data recognises.
#[derive(Debug, Clone)]
struct Now {
    kind: &'static str,
    /// What its version resource says, as 0.5.1 shows it.
    version: Option<String>,
    preloader: &'static str,
    /// The row that makes `version` differ from 0.5.0's, when it does.
    version_row: Option<Row>,
}

/// A source 0.5.1 lists where 0.5.0 lists none: what its row says besides its loader.
#[derive(Debug, Clone)]
struct Appear {
    manager: &'static str,
    profile: String,
    root: PathBuf,
}

/// What 0.5.1's answer for one BepInEx source is, by the rows.
#[derive(Debug, Default, Clone)]
struct Effect {
    /// Mods 0.5.1 counts more (+) or fewer (-), and the rows that say so.
    delta: i64,
    rows: Vec<Row>,
    /// 0.5.1 lists no BepInEx here, for this row.
    absent: Option<Row>,
    /// The loader 0.5.1 reads (a data kind's).
    now: Option<Now>,
    /// More mods than one evaluation reads.
    over: bool,
    /// 0.5.0 lists no source here (no mod it counts) and 0.5.1 does.
    appears: Option<Appear>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavour {
    Mono,
    Il2cpp,
    Unknown,
}

/// The BepInEx (or other loader) laid in one folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lay {
    Bep5(&'static str),
    Bep6(&'static str),
    Bep6NeverRun(&'static str),
    /// Both cores, BepInEx 5's and BepInEx 6 IL2CPP's, in one `BepInEx/core`.
    Mixed(&'static str, &'static str),
    Bep6Mono,
    Bep6Coreclr,
    Bep6OldName,
    LibraryMissing,
    Melon,
    NoCore,
    Nothing,
}

const BEP5_VERSIONS: [&str; 5] = [
    "5.4.23.2+b0f5d7e9a5a8d4f2c6b1e3a7d9c0b2a4e6f8a1c3",
    "5.4.21.0",
    "5.4.22.0",
    "5.4.23.3",
    "5.3.0.0",
];
const BEP6_VERSIONS: [&str; 3] = [
    "6.0.0-be.788+5b766a3b7f6c164d4798924a93f3acf4db769d06",
    "6.0.0-be.735+5fef357",
    "6.0.0-dev+3a12d1810bf4dd5ca1635352ccf5bff5d95f987f",
];

/// What an installed integration says about mods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Install {
    None,
    Mono,
    Il2cpp(u32),
    NoTable,
    UnknownKey,
    BadBuild,
    NoDoorstop,
    Native,
    PerGameBepinex,
}

struct World {
    tmp: Tmp,
    rng: Rng,
    flavour: Flavour,
    /// The program below the install folder (`Game/Some Game.exe`), not at its top.
    below: bool,
    appid: u32,
    windows_env: bool,
    install: Install,
    added: Vec<PathBuf>,
    expect: BTreeMap<PathBuf, Effect>,
    /// Links laid so far: each one's target folder is its own.
    links: u32,
    /// The corpus' file system tells `BepInEx` from `bepinex` (Linux; not Windows, not macOS).
    case_sensitive: bool,
}

impl World {
    fn dir(&self) -> PathBuf {
        self.tmp.join("common").join(GAME)
    }

    /// `${game}`: the folder holding the program.
    fn game_root(&self) -> PathBuf {
        if self.below {
            self.dir().join("Game")
        } else {
            self.dir()
        }
    }

    fn exe(&self) -> String {
        if self.below {
            format!("Game/{GAME}.exe")
        } else {
            format!("{GAME}.exe")
        }
    }

    fn home(&self) -> PathBuf {
        self.tmp.join("home")
    }

    fn profile(&self) -> PathBuf {
        self.tmp.join("profiles/p")
    }

    fn platform(&self) -> Platform {
        if self.install == Install::Native {
            Platform::LinuxNative
        } else {
            Platform::Windows
        }
    }

    fn target(&self) -> now::GameTarget {
        target(&self.dir(), &self.exe(), Some(self.appid), self.platform())
    }

    fn env(&self) -> now::ModsEnv {
        if self.windows_env {
            now::ModsEnv::windows(Some(self.home().join("AppData/Roaming")))
        } else {
            now::ModsEnv::linux(Some(&self.home()), None, None)
        }
    }

    /// Each manager's data folder for this world, with its name.
    fn managers(&self) -> Vec<(&'static str, PathBuf)> {
        if self.windows_env {
            let a = self.home().join("AppData/Roaming");
            vec![
                ("r2modman", a.join("r2modmanPlus-local")),
                (
                    "thunderstore",
                    a.join("Thunderstore Mod Manager/DataFolder"),
                ),
                ("gale", a.join("com.kesomannen.gale")),
            ]
        } else {
            vec![
                ("r2modman", self.home().join(".config/r2modmanPlus-local")),
                ("gale", self.home().join(".local/share/com.kesomannen.gale")),
                (
                    "r2modman",
                    self.home()
                        .join(".var/app/com.github.ebkr.r2modmanPlus/config/r2modmanPlus-local"),
                ),
            ]
        }
    }
}

/// `rel` below `root` as a canonical key.
fn key(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The game folder of `flavour` at `at`.
fn lay_game(at: &Path, flavour: Flavour) {
    write(
        &at.join(format!("{GAME}.exe")),
        &pe_with_options(X64, &[], b"game"),
    );
    write(&at.join("UnityPlayer.dll"), b"MZ player");
    match flavour {
        Flavour::Mono => write(
            &at.join(format!("{GAME}_Data/Managed/Assembly-CSharp.dll")),
            b"MZ csharp",
        ),
        Flavour::Il2cpp => {
            write(&at.join("GameAssembly.dll"), b"MZ il2cpp");
            write(
                &at.join(format!(
                    "{GAME}_Data/il2cpp_data/Metadata/global-metadata.dat"
                )),
                b"meta",
            );
        }
        Flavour::Unknown => write(&at.join(format!("{GAME}_Data/resources.assets")), b"x"),
    }
}

/// The integration of `install`, for a game of `flavour`.
fn package(install: Install, flavour: Flavour) -> Option<now::GiPackage> {
    let engine = match flavour {
        Flavour::Mono => "unity-mono",
        Flavour::Il2cpp => "unity-il2cpp",
        Flavour::Unknown => return None,
    };
    let proxy = pe_with_options(X64, DOORSTOP_V4, b"4.5.0 ours");
    let decl = match install {
        Install::None => return None,
        Install::Mono | Install::Native => MODS_DECL.to_owned(),
        Install::Il2cpp(build) => MODS_DECL.replace(
            "loader = \"bepinex5\"",
            &format!("loader = \"bepinex6-il2cpp\"\nbepinex_build = {build}"),
        ),
        Install::NoTable => String::new(),
        Install::UnknownKey => format!("{MODS_DECL}future = 1\n"),
        Install::BadBuild => format!("{MODS_DECL}bepinex_build = 1\n"),
        Install::NoDoorstop => MODS_DECL.to_owned(),
        Install::PerGameBepinex => {
            // Astra's own BepInEx placed in the game folder: never one of the player's sources.
            let core = pe_with_resource(&[("ProductVersion", "5.4.23.2")]);
            let m = runner_manifest(&format!(
                "\n[[game_files]]\nfrom = \"game/BepInEx/core/BepInEx.Preloader.dll\"\nto = \"${{game}}/BepInEx/core/BepInEx.Preloader.dll\"\n\n[[game_files]]\nfrom = \"game/BepInEx/core/BepInEx.dll\"\nto = \"${{game}}/BepInEx/core/BepInEx.dll\"\n\n{MODS_DECL}"
            ))
            .replace(&sha(WINHTTP), &sha(&proxy))
            .replace("engine = \"unity-mono\"", &format!("engine = \"{engine}\""));
            return Some(
                open(&zip(&[
                    E::File("astra-gi.toml", m.as_bytes()),
                    E::File("winhttp.dll", &proxy),
                    E::File("doorstop_config.ini", DOORSTOP_INI),
                    E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
                    E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
                    E::File("game/BepInEx/core/BepInEx.Preloader.dll", b"MZ ours"),
                    E::File("game/BepInEx/core/BepInEx.dll", &core),
                ]))
                .unwrap(),
            );
        }
    };
    if install == Install::NoDoorstop {
        let m = runner_manifest(&format!(
            "\n[[files]]\nfrom = \"chain/**\"\nto = \"${{profile}}/chain/\"\n\n{decl}"
        ))
        .replace(&sha(WINHTTP), &sha(&proxy))
        .replace("engine = \"unity-mono\"", &format!("engine = \"{engine}\""));
        let m = m.replace(
            "args = [\"--doorstop-enabled\", \"true\", \"--doorstop-target-assembly\", \"${profile}/BepInEx/core/BepInEx.Preloader.dll\"]",
            "args = [\"-batchmode\"]",
        );
        return Some(
            open(&zip(&[
                E::File("astra-gi.toml", m.as_bytes()),
                E::File("winhttp.dll", &proxy),
                E::File("doorstop_config.ini", DOORSTOP_INI),
                E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
                E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
                E::File("chain/Astra.Chain.dll", CHAIN_DLL),
            ]))
            .unwrap(),
        );
    }
    Some(mods_runner_for(&proxy, &decl, engine))
}

/// The plugins of one BepInEx source, laid below `root`; what 0.5.1 counts otherwise (for a
/// loader the data reads, `data`). Returns the mods 0.5.0 counts and those 0.5.1 counts.
fn lay_plugins(
    w: &mut World,
    root: &Path,
    manager_kept: bool,
    data: bool,
    eff: &mut Effect,
) -> (i64, i64) {
    let r = &mut w.rng;
    let mut dll_mods = 0;
    let mut plus = 0;
    for i in 0..r.below(4) {
        let name = format!("Author-Mod{i}");
        if r.chance(25) {
            write(
                &root.join(format!("BepInEx/plugins/{name}/sub/plugins/Mod{i}.dll")),
                b"MZ a mod",
            );
        } else {
            write(
                &root.join(format!("BepInEx/plugins/{name}/Mod{i}.dll")),
                b"MZ a mod",
            );
        }
        if r.chance(40) {
            write(
                &root.join(format!("BepInEx/plugins/{name}/manifest.json")),
                b"{}",
            );
            write(
                &root.join(format!("BepInEx/plugins/{name}/README.md")),
                b"# mod",
            );
        }
        if r.chance(15) {
            // Half switched off: still a mod BepInEx loads.
            write(
                &root.join(format!("BepInEx/plugins/{name}/Extra.dll.old")),
                b"MZ",
            );
        }
        dll_mods += 1;
    }
    for name in ["Loose.dll", "UPPER.DLL"] {
        if r.chance(30) {
            write(&root.join("BepInEx/plugins").join(name), b"MZ loose");
            dll_mods += 1;
        }
    }
    if r.chance(25) {
        write(&root.join("BepInEx/plugins/LooseOff.dll.old"), b"MZ");
    }
    if r.chance(20) {
        write(&root.join("BepInEx/plugins/notes.txt"), b"notes");
    }
    // r2modman's "disable": the mod's files renamed `*.old`, its folder kept.
    if r.chance(30) {
        write(&root.join("BepInEx/plugins/Author-Off/Off.dll.old"), b"MZ");
        write(
            &root.join("BepInEx/plugins/Author-Off/manifest.json.old"),
            b"{}",
        );
        if !manager_kept {
            plus += 1;
        }
    }
    // A pack of textures another mod reads; an empty folder.
    if r.chance(20) {
        write(
            &root.join("BepInEx/plugins/Author-Suits/moresuits/suit.png"),
            b"png",
        );
        plus += 1;
    }
    if r.chance(10) {
        fs::create_dir_all(root.join("BepInEx/plugins/Empty")).unwrap();
        plus += 1;
    }
    // Astra's own item: never counted, by either.
    if r.chance(20) {
        write(
            &root.join("BepInEx/plugins/Astra/Astra.Bridge.dll"),
            b"MZ ours",
        );
        write(&root.join("BepInEx/plugins/Astra/astra-item.json"), b"{}");
    }
    let mut patchers = 0;
    if r.chance(25) {
        write(&root.join("BepInEx/patchers/Author-Patch/Patch.dll"), b"MZ");
        patchers += 1;
        if r.chance(40) {
            write(&root.join("BepInEx/patchers/Loose.Patcher.dll"), b"MZ");
            patchers += 1;
        }
        if r.chance(30) {
            write(&root.join("BepInEx/patchers/Off/Off.dll.old"), b"MZ");
        }
    }
    if r.chance(40) {
        write(&root.join("BepInEx/config/BepInEx.cfg"), b"[Logging]\n");
    }
    // A mod folder and a loose dll that are links, their targets outside the source: 0.5.0
    // followed them, the data never does.
    let mut links = 0;
    if cfg!(unix) && data && w.rng.chance(8) {
        w.links += 1;
        let target = w.tmp.join(&format!("dev/{}", w.links));
        write(&target.join("Linked/Linked.dll"), b"MZ a linked mod");
        write(&target.join("LinkedLoose.dll"), b"MZ a linked loose mod");
        fs::create_dir_all(root.join("BepInEx/plugins")).unwrap();
        if w.rng.chance(60) {
            symlink(&target.join("Linked"), &root.join("BepInEx/plugins/Linked"));
            links += 1;
        }
        if links == 0 || w.rng.chance(50) {
            symlink(
                &target.join("LinkedLoose.dll"),
                &root.join("BepInEx/plugins/LinkedLoose.dll"),
            );
            links += 1;
        }
    }
    // More mods than one evaluation reads (§7.8: 1,024): 0.5.0 counted them.
    let mut bulk = 0;
    if data && w.rng.chance(2) {
        for i in 0..1_025 {
            write(&root.join(format!("BepInEx/plugins/Bulk{i:04}.dll")), b"MZ");
        }
        bulk = 1_025;
        eff.over = true;
    }
    if plus > 0 {
        eff.rows.push(Row::FolderWithoutDll);
    }
    if patchers > 0 {
        eff.rows.push(Row::Patchers);
    }
    if links > 0 {
        eff.rows.push(Row::Link);
    }
    let then = dll_mods + patchers + links + bulk;
    let now = dll_mods + plus + bulk;
    eff.delta += now - then;
    (then, now)
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path) {
    unreachable!("links are laid on Unix only");
}

/// BepInEx's log at `root`: its start line naming `program` (or another), written `ago` ago.
fn lay_log(w: &mut World, root: &Path, program: &str) {
    let r = &mut w.rng;
    if !r.chance(70) {
        return;
    }
    let mut lines = Vec::new();
    if r.chance(30) {
        lines.push("[Error  : Unity Log] Unable to start Unity log writer".to_owned());
    }
    lines.push(format!(
        "[Message:   BepInEx] BepInEx 5.4.23.2 - {program} (10/30/2026 1:23:45 PM)"
    ));
    let p = root.join("BepInEx/LogOutput.log");
    write(&p, lines.join("\n").as_bytes());
    let ago = Duration::from_secs(60 * (1 + r.below(10_000)));
    fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(SystemTime::now() - ago)
        .unwrap();
}

/// The version a data kind's library says, written: `ProductVersion` as `v`; or, for the rows,
/// a resource with no `ProductVersion` (or an empty one) and `FileVersion`, or one whose
/// `ProductVersion` holds control characters. What 0.5.1 shows of it, and the row.
fn library_version(w: &mut World, at: &Path, name: &str, v: &str) -> (Option<String>, Option<Row>) {
    let short = v.split('+').next().unwrap().to_owned();
    if w.rng.chance(5) {
        let product = if w.rng.chance(50) {
            vec![("ProductName", name), ("FileVersion", short.as_str())]
        } else {
            vec![
                ("ProductName", name),
                ("ProductVersion", ""),
                ("FileVersion", short.as_str()),
            ]
        };
        write(at, &pe_with_resource(&product));
        return (Some(short), Some(Row::VersionFallback));
    }
    if w.rng.chance(4) {
        let spoken = format!("{v}\u{1b}[31m\u{7}");
        write(
            at,
            &pe_with_resource(&[("ProductName", name), ("ProductVersion", spoken.as_str())]),
        );
        return (Some(format!("{v}[31m")), Some(Row::Printable));
    }
    (Some(v.to_owned()), None)
}

/// One loader of the player's laid at `root` (a profile, a picked folder, the game folder), kept
/// by `manager` (its code as a row says it), and what 0.5.1 answers for it otherwise, keyed by
/// its folder.
fn lay_source(
    w: &mut World,
    root: &Path,
    lay: Lay,
    manager: &'static str,
    manager_kept: bool,
    game_folder: bool,
) {
    let mut eff = Effect::default();
    let flavour = w.flavour;
    let bep5 = |v: Option<String>, row| Now {
        kind: "bepinex5",
        version: v,
        preloader: "BepInEx.Preloader.dll",
        version_row: row,
    };
    let bep6 = |v: Option<String>, row| Now {
        kind: "bepinex6-il2cpp",
        version: v,
        preloader: "BepInEx.Unity.IL2CPP.dll",
        version_row: row,
    };
    match lay {
        Lay::Bep5(v) => {
            bepinex5_at(root, v);
            let mut written = (Some(v.to_owned()), None);
            if w.rng.chance(30) {
                write(
                    &root.join("BepInEx/core/BepInEx.dll"),
                    &pe_with_resource(&[
                        ("ProductName", "BepInEx"),
                        ("ProductVersion", v),
                        ("FileVersion", "5.4.23.2"),
                    ]),
                );
            } else {
                written = library_version(w, &root.join("BepInEx/core/BepInEx.dll"), "BepInEx", v);
            }
            eff.now = Some(bep5(written.0, written.1));
            if flavour == Flavour::Il2cpp {
                eff.absent = Some(Row::AnotherFlavour);
            }
        }
        Lay::Bep6(v) | Lay::Bep6NeverRun(v) => {
            bepinex6_il2cpp_at(root, v);
            let written = library_version(
                w,
                &root.join("BepInEx/core/BepInEx.Core.dll"),
                "BepInEx.Core",
                v,
            );
            eff.now = Some(bep6(written.0, written.1));
            if lay == Lay::Bep6NeverRun(v) {
                fs::remove_dir_all(root.join("BepInEx/interop")).unwrap();
                eff.absent = Some(Row::Il2cppNeverRun);
            }
            if flavour == Flavour::Mono {
                eff.absent = Some(Row::AnotherFlavour);
            }
        }
        Lay::Mixed(v5, v6) => {
            bepinex5_at(root, v5);
            bepinex6_il2cpp_at(root, v6);
            // Each flavour's data reads its own core.
            eff.now = Some(if flavour == Flavour::Il2cpp {
                bep6(Some(v6.to_owned()), None)
            } else {
                bep5(Some(v5.to_owned()), None)
            });
        }
        Lay::Bep6Mono => {
            write(
                &root.join("BepInEx/core/BepInEx.Unity.Mono.Preloader.dll"),
                b"MZ",
            );
            write(
                &root.join("BepInEx/core/BepInEx.Core.dll"),
                &pe_with_resource(&[("ProductVersion", "6.0.0-be.697+5362580")]),
            );
        }
        Lay::Bep6Coreclr => {
            write(&root.join("BepInEx/core/BepInEx.NET.CoreCLR.dll"), b"MZ");
            write(
                &root.join("BepInEx/core/BepInEx.Core.dll"),
                &pe_with_resource(&[("ProductVersion", "6.0.0-be.725+e1974e2")]),
            );
        }
        Lay::Bep6OldName => {
            write(&root.join("BepInEx/core/BepInEx.IL2CPP.dll"), b"MZ");
            write(
                &root.join("BepInEx/core/BepInEx.Core.dll"),
                &pe_with_resource(&[("ProductVersion", "6.0.0-be.566+8e1b6c4")]),
            );
            write(&root.join("BepInEx/unhollowed/Assembly-CSharp.dll"), b"MZ");
        }
        Lay::LibraryMissing => {
            write(
                &root.join("BepInEx/core/BepInEx.Preloader.dll"),
                b"MZ preloader",
            );
            eff.absent = Some(Row::LibraryMissing);
        }
        Lay::Melon => {
            for d in ["MelonLoader/net6", "Plugins", "UserData"] {
                fs::create_dir_all(root.join(d)).unwrap();
            }
            write(&root.join("MelonLoader/net6/MelonLoader.dll"), b"MZ");
            for i in 0..w.rng.below(3) {
                write(&root.join(format!("Mods/Melon{i}.dll")), b"MZ");
            }
            if game_folder {
                write(&root.join("version.dll"), b"MZ melon proxy");
            }
            return;
        }
        Lay::NoCore => {
            plugin_at(root, "A-Mod/A.dll");
            return;
        }
        Lay::Nothing => {
            fs::create_dir_all(root).unwrap();
            return;
        }
    }
    let data_kind = matches!(
        lay,
        Lay::Bep5(_) | Lay::Bep6(_) | Lay::Bep6NeverRun(_) | Lay::Mixed(..)
    );
    if data_kind && flavour == Flavour::Unknown {
        eff.absent = Some(Row::FlavourUnknown);
    }
    if data_kind && game_folder && w.below {
        eff.absent = Some(Row::ProgramBelowTheFolder);
    }
    let (then, now) = if data_kind {
        lay_plugins(w, root, manager_kept, true, &mut eff)
    } else {
        // 0.5.0's own count, for a BepInEx the data does not describe.
        let mut ignored = Effect::default();
        lay_plugins(w, root, manager_kept, false, &mut ignored)
    };
    lay_log(w, root, GAME);
    if data_kind && eff.absent.is_none() {
        // What the data never reads, laid last over the whole tree: a `BepInEx/` (or its
        // `core/`) spelled in another case, a `BepInEx/` that is a link.
        if w.case_sensitive && w.rng.chance(4) {
            if w.rng.chance(50) {
                fs::rename(root.join("BepInEx"), root.join("bepinex")).unwrap();
            } else {
                fs::rename(root.join("BepInEx/core"), root.join("BepInEx/Core")).unwrap();
            }
            eff.absent = Some(Row::CaseVariant);
        } else if cfg!(unix) && w.rng.chance(2) {
            w.links += 1;
            let target = w.tmp.join(&format!("dev/{}/BepInEx", w.links));
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::rename(root.join("BepInEx"), &target).unwrap();
            symlink(&target, &root.join("BepInEx"));
            eff.absent = Some(Row::Link);
        }
    }
    if data_kind && eff.absent.is_none() && then == 0 && now > 0 {
        eff.appears = Some(Appear {
            manager,
            profile: if game_folder {
                String::new()
            } else {
                root.file_name().unwrap().to_string_lossy().into_owned()
            },
            root: root.to_path_buf(),
        });
    }
    w.expect.insert(key(root), eff);
}

/// A loader kind for a profile of this world.
fn pick_lay(r: &mut Rng, flavour: Flavour) -> Lay {
    let n = r.below(100);
    match (flavour, n) {
        (Flavour::Il2cpp, 0..=44) => Lay::Bep6(r.pick(&BEP6_VERSIONS)),
        (Flavour::Il2cpp, 45..=52) => Lay::Bep6NeverRun(BEP6_VERSIONS[0]),
        (Flavour::Il2cpp, 53..=58) => Lay::Bep5(r.pick(&BEP5_VERSIONS)),
        (Flavour::Il2cpp, 59..=62) => Lay::Mixed(r.pick(&BEP5_VERSIONS), r.pick(&BEP6_VERSIONS)),
        (_, 0..=54) => Lay::Bep5(r.pick(&BEP5_VERSIONS)),
        (_, 55..=60) => Lay::Bep6(r.pick(&BEP6_VERSIONS)),
        (_, 61..=66) => Lay::Bep6Mono,
        (_, 67..=71) => Lay::Bep6Coreclr,
        (_, 72..=75) => Lay::Bep6OldName,
        (_, 76..=80) => Lay::LibraryMissing,
        (_, 81..=88) => Lay::Melon,
        (_, 89..=92) => Lay::NoCore,
        (_, 93..=95) => Lay::Mixed(r.pick(&BEP5_VERSIONS), r.pick(&BEP6_VERSIONS)),
        _ => Lay::Nothing,
    }
}

/// World `i` of the corpus, laid below `TMPDIR`.
fn world(i: u64) -> World {
    let mut rng = Rng::new(SEED.wrapping_add(i.wrapping_mul(0x9E37_79B9)));
    let flavour = match rng.below(100) {
        0..=54 => Flavour::Mono,
        55..=89 => Flavour::Il2cpp,
        _ => Flavour::Unknown,
    };
    let below = rng.chance(12);
    let appid = if rng.chance(35) { LETHAL } else { NEW_APPID };
    let windows_env = rng.chance(40);
    let install = match (flavour, rng.below(100)) {
        (Flavour::Unknown, _) => Install::None,
        (_, 0..=14) => Install::None,
        (Flavour::Mono, 15..=54) => Install::Mono,
        (Flavour::Mono, 55..=62) => Install::PerGameBepinex,
        (Flavour::Il2cpp, 15..=49) => Install::Il2cpp(788),
        (Flavour::Il2cpp, 50..=62) => Install::Il2cpp(735),
        (_, 63..=74) => Install::NoTable,
        (_, 75..=80) => Install::UnknownKey,
        (_, 81..=86) => Install::BadBuild,
        (_, 87..=92) => Install::NoDoorstop,
        _ => Install::Native,
    };
    let mut w = World {
        tmp: Tmp::new(&format!("mods-id-{i}")),
        rng,
        flavour,
        below,
        appid,
        windows_env,
        install,
        added: Vec::new(),
        expect: BTreeMap::new(),
        links: 0,
        case_sensitive: false,
    };
    w.case_sensitive = {
        write(&w.tmp.join("CaseProbe"), b"");
        fs::metadata(w.tmp.join("caseprobe")).is_err()
    };
    fs::remove_file(w.tmp.join("CaseProbe")).unwrap();
    lay_game(&w.game_root(), flavour);
    fs::create_dir_all(w.home()).unwrap();
    if let Some(pkg) = package(install, flavour) {
        plan_and_install(&pkg, &w.target(), &w.profile());
    } else {
        fs::create_dir_all(w.profile()).unwrap();
    }

    // The managers' profiles: under the game's own folder, a folder a log names, decoys.
    let (r2_name, gale_name) = if appid == LETHAL {
        ("LethalCompany", "lethal-company")
    } else {
        (GAME, GAME)
    };
    for (manager, data) in w.managers() {
        if !w.rng.chance(60) {
            continue;
        }
        let folder = if manager == "gale" {
            gale_name
        } else {
            r2_name
        };
        for p in 0..w.rng.below(4) {
            let root = data.join(folder).join("profiles").join(format!("P{p}"));
            let lay = pick_lay(&mut w.rng, flavour);
            lay_source(&mut w, &root, lay, manager, true, false);
        }
        if appid == NEW_APPID && w.rng.chance(40) {
            let root = data.join("renamed").join("profiles").join("ByLog");
            let lay = pick_lay(&mut w.rng, flavour);
            lay_source(&mut w, &root, lay, manager, true, false);
            if fs::metadata(root.join("BepInEx")).is_ok() {
                write(
                    &root.join("BepInEx/LogOutput.log"),
                    format!("[Message:   BepInEx] BepInEx 5.4.23.2 - {GAME} (x)").as_bytes(),
                );
            }
        }
        if w.rng.chance(30) {
            // Another game's profiles: never this game's.
            let root = data.join("valheim").join("profiles").join("Default");
            bepinex5_at(&root, "5.4.23.2");
            plugin_at(&root, "V/V.dll");
        }
    }

    // The game folder: a BepInEx installed by hand (not over Astra's own), MelonLoader.
    let game_root = w.game_root();
    if w.rng.chance(35) && install != Install::PerGameBepinex {
        let lay = match flavour {
            Flavour::Il2cpp => Lay::Bep6(w.rng.pick(&BEP6_VERSIONS)),
            _ => Lay::Bep5(w.rng.pick(&BEP5_VERSIONS)),
        };
        lay_source(&mut w, &game_root, lay, "manual", false, true);
        write(&game_root.join(".doorstop_version"), b"4.3.0");
        if install == Install::None {
            write(&game_root.join("winhttp.dll"), b"MZ their doorstop");
            write(
                &game_root.join("doorstop_config.ini"),
                b"[General]\nenabled = true\n",
            );
        }
    } else if install == Install::PerGameBepinex && w.rng.chance(50) {
        // The player's plugins beside Astra's own BepInEx: still Astra's BepInEx.
        plugin_at(&game_root, "Their/Their.dll");
    }
    if w.rng.chance(15) {
        lay_source(&mut w, &game_root, Lay::Melon, "manual", false, true);
    }

    // Folders the player picked: a folder of profiles, a profile itself, one inside a Gale data
    // folder kept elsewhere (Gale's adapter reads it), a profile a manager already lists.
    let mut added = Vec::new();
    if w.rng.chance(25) {
        let base = w.tmp.join("picked/My mods");
        let root = base.join("profiles/Mine");
        let lay = pick_lay(&mut w.rng, flavour);
        lay_source(&mut w, &root, lay, "folder", false, false);
        added.push(base);
    }
    if w.rng.chance(20) {
        let root = w.tmp.join("picked/Direct");
        let lay = pick_lay(&mut w.rng, flavour);
        lay_source(&mut w, &root, lay, "folder", false, false);
        added.push(root);
    }
    if w.rng.chance(15) {
        let base = w.tmp.join("D/com.kesomannen.gale/moved");
        let root = base.join("profiles/Kept");
        let lay = pick_lay(&mut w.rng, flavour);
        lay_source(&mut w, &root, lay, "gale", true, false);
        added.push(base);
    }
    if w.rng.chance(10)
        && let Some(listed) = w
            .expect
            .keys()
            .find(|k| k.to_string_lossy().contains("profiles"))
    {
        added.push(listed.clone());
    }
    for a in &added {
        now::add_mods_root(&w.profile(), a).unwrap();
    }
    w.added = added;
    w
}

/// Launcher 0.5.0's verdict on a BepInEx of `kind` whose library says `version`, in world `w`:
/// the installed integration's capability, then `accepts` (BepInEx 5.4.x for `bepinex5`, exactly
/// the declared build for `bepinex6-il2cpp`). Held to every verdict 0.5.0 gives over the corpus
/// (`expected`), so it is 0.5.0's rule wherever a row changes the loader 0.5.1 reads.
fn verdict_050(w: &World, kind: &str, version: Option<&str>) -> Value {
    let not =
        |reason: &str| serde_json::json!({"Err": {"code": "not-supported", "reason": reason}});
    let (want, need) = match w.install {
        Install::None => return not("not-installed"),
        Install::NoTable => return serde_json::json!({"Err": {"code": "integration-without-mods"}}),
        Install::UnknownKey => return not("unknown-key"),
        Install::BadBuild => return not("manifest-invalid"),
        Install::Native => return not("platform"),
        Install::NoDoorstop => return not("no-doorstop"),
        Install::Mono | Install::PerGameBepinex => (None, "5.4".to_owned()),
        Install::Il2cpp(b) => (Some(b), format!("6.0.0-be.{b}")),
    };
    let ok = match want {
        None => kind == "bepinex5" && version.is_some_and(|v| v.starts_with("5.4.")),
        Some(b) => {
            kind == "bepinex6-il2cpp"
                && version
                    .and_then(|v| v.strip_prefix("6.0.0-be."))
                    .map(|r| {
                        r.chars()
                            .take_while(char::is_ascii_digit)
                            .collect::<String>()
                    })
                    .and_then(|d| d.parse::<u32>().ok())
                    == Some(b)
        }
    };
    if ok {
        return serde_json::json!({"Ok": null});
    }
    let have = version.map_or(kind, |v| v.split('+').next().unwrap());
    serde_json::json!({"Err": {"code": "loader-unsupported", "have": have, "need": need}})
}

/// 0.5.1's verdict on a loader of a data kind: 0.5.0's, but for [`Row::BuildNot788`].
fn verdict_051(w: &World, kind: &str, version: Option<&str>) -> (Value, bool) {
    let then = verdict_050(w, kind, version);
    let passed = then.get("Ok").is_some() || then["Err"]["code"] == "loader-unsupported";
    match w.install {
        Install::Il2cpp(b) if b != 788 && kind == "bepinex6-il2cpp" && passed => (
            serde_json::json!({"Err": {"code": "not-supported", "reason": "outside-versions"}}),
            true,
        ),
        _ => (then, false),
    }
}

/// Discovery's order: the most recently played first, then profile, manager, root; one folder's
/// BepInEx before its MelonLoader.
fn discovery_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    let manager = |v: &Value| {
        ["r2modman", "thunderstore", "gale", "manual", "folder"]
            .iter()
            .position(|m| v["manager"] == *m)
            .unwrap()
    };
    let melon = |v: &Value| v["loader"]["kind"] == "melonloader";
    b["last_played"]
        .as_u64()
        .cmp(&a["last_played"].as_u64())
        .then_with(|| a["profile"].as_str().cmp(&b["profile"].as_str()))
        .then_with(|| manager(a).cmp(&manager(b)))
        .then_with(|| {
            Path::new(a["root"].as_str().unwrap()).cmp(Path::new(b["root"].as_str().unwrap()))
        })
        .then_with(|| melon(a).cmp(&melon(b)))
}

/// When BepInEx's log at `root` was written, as a row says it.
fn log_ms(root: &Path) -> Option<u64> {
    let t = fs::metadata(root.join("BepInEx/LogOutput.log"))
        .ok()?
        .modified()
        .ok()?;
    u64::try_from(t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// 0.5.0's answer as 0.5.1 gives it, by the rows; which rows applied, and how many of 0.5.0's
/// sources no row touched (`untouched`: compared exactly as 0.5.0 gave them). Every verdict 0.5.0
/// gives a BepInEx the data reads is held to [`verdict_050`] (`wrong_050`).
fn expected(
    w: &World,
    then: &[launcher_050::ModSource],
    used: &mut BTreeMap<Row, usize>,
    untouched: &mut usize,
    wrong_050: &mut Vec<String>,
) -> Vec<Value> {
    let mut out = Vec::new();
    for s in then {
        let mut v = serde_json::to_value(s).unwrap();
        let mut touched = false;
        let bepinex = matches!(
            s.loader.kind,
            launcher_050::LoaderKind::Bepinex5 | launcher_050::LoaderKind::Bepinex6Il2cpp
        ) && s.loader.preloader.as_deref().is_some_and(|p| {
            p.eq_ignore_ascii_case("BepInEx.Preloader.dll")
                || p.eq_ignore_ascii_case("BepInEx.Unity.IL2CPP.dll")
        });
        if bepinex {
            let kind = v["loader"]["kind"].as_str().unwrap().to_owned();
            let rule = verdict_050(w, &kind, s.loader.version.as_deref());
            if rule != v["playable"] {
                wrong_050.push(format!(
                    "{}: 0.5.0 says {}, verdict_050 {rule}",
                    s.root.display(),
                    v["playable"]
                ));
            }
        }
        if bepinex && let Some(e) = w.expect.get(&key(&s.root)) {
            touched = e.absent.is_some() || !e.rows.is_empty();
            if let Some(row) = e.absent {
                *used.entry(row).or_default() += 1;
                continue;
            }
            if e.over {
                *used.entry(Row::OverBudget).or_default() += 1;
                // The loader the data reads, nothing of it listed.
                let now = e.now.as_ref().expect("a data kind's loader");
                v["loader"]["kind"] = Value::from(now.kind);
                v["mods"] = Value::from(0);
                v["loader"]["version"] = Value::Null;
                v["loader"]["preloader"] = Value::Null;
                v["playable"] =
                    serde_json::json!({"Err": {"code": "not-supported", "reason": "over-budget"}});
                out.push(v);
                continue;
            }
            for row in &e.rows {
                *used.entry(*row).or_default() += 1;
            }
            let mods = i64::from(s.mods) + e.delta;
            if mods <= 0 {
                continue;
            }
            v["mods"] = Value::from(mods);
            let now = e.now.as_ref().expect("a data kind's loader");
            if v["loader"]["kind"] != now.kind {
                *used.entry(Row::MixedCores).or_default() += 1;
                v["loader"]["kind"] = Value::from(now.kind);
                v["loader"]["preloader"] = Value::from(now.preloader);
                v["loader"]["version"] = serde_json::to_value(&now.version).unwrap();
                touched = true;
            } else if v["loader"]["version"].as_str() != now.version.as_deref() {
                let row = now
                    .version_row
                    .expect("a version 0.5.1 reads otherwise, by a row");
                *used.entry(row).or_default() += 1;
                v["loader"]["version"] = serde_json::to_value(&now.version).unwrap();
                touched = true;
            }
            let (p, by_build) = verdict_051(w, now.kind, now.version.as_deref());
            if by_build {
                *used.entry(Row::BuildNot788).or_default() += 1;
                touched = true;
            }
            v["playable"] = p;
        }
        if !touched {
            *untouched += 1;
        }
        out.push(v);
    }
    // The sources 0.5.1 lists where 0.5.0 listed none.
    for e in w.expect.values() {
        let Some(a) = &e.appears else { continue };
        let now = e.now.as_ref().expect("a data kind's loader");
        *used.entry(Row::SourceAppears).or_default() += 1;
        let (playable, _) = verdict_051(w, now.kind, now.version.as_deref());
        out.push(serde_json::json!({
            "manager": a.manager,
            "profile": a.profile,
            "root": a.root,
            "loader": {"kind": now.kind, "version": now.version, "preloader": now.preloader},
            "mods": e.delta,
            "last_played": log_ms(&a.root),
            "playable": playable,
        }));
    }
    out.sort_by(discovery_order);
    out
}

/// The verdict's name, for the floors: `ok`, the code, `not-supported/<reason>`.
fn verdict(v: &Value) -> String {
    match &v["playable"] {
        Value::Object(o) if o.contains_key("Ok") => "ok".to_owned(),
        Value::Object(o) => {
            let e = &o["Err"];
            let code = e["code"].as_str().unwrap();
            match e.get("reason").and_then(Value::as_str) {
                Some(r) => format!("{code}/{r}"),
                None => code.to_owned(),
            }
        }
        other => panic!("{other}"),
    }
}

#[test]
fn mods_discovery_answers_as_launcher_0_5_0_over_the_corpus() {
    let worlds = std::env::var(WORLDS_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(WORLDS);
    let mut used: BTreeMap<Row, usize> = BTreeMap::new();
    let mut verdicts: BTreeMap<String, usize> = BTreeMap::new();
    let mut managers: BTreeMap<String, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut sources = 0usize;
    let mut untouched = 0usize;
    let mut wrong_050 = Vec::new();
    let mut failures = Vec::new();
    let mut case_sensitive = true;
    for i in 0..worlds {
        let w = world(i);
        case_sensitive &= w.case_sensitive;
        let then_target: launcher_050::GameTarget =
            serde_json::from_value(serde_json::to_value(w.target()).unwrap()).unwrap();
        let env = w.env();
        let then_env = launcher_050::ModsEnv {
            appdata: env.appdata.clone(),
            config_home: env.config_home.clone(),
            data_home: env.data_home.clone(),
            flatpak_apps: env.flatpak_apps.clone(),
        };
        let before = snapshot(w.tmp.path());
        let then = launcher_050::discover_mods_in(&then_target, &w.profile(), &then_env);
        let ours = now::discover_mods_in(&w.target(), &w.profile(), &env);
        assert_eq!(snapshot(w.tmp.path()), before, "world {i}: discovery wrote");
        let want = expected(&w, &then, &mut used, &mut untouched, &mut wrong_050);
        let got: Vec<Value> = ours
            .iter()
            .map(|s| {
                // How Astra runs beside it: the data's chain for its BepInEx (one over budget
                // too: the data's, though nothing of it is listed), nothing for the rest. Not a
                // field 0.5.0 had.
                let data = matches!(
                    s.loader.kind,
                    now::LoaderKind::Bepinex5 | now::LoaderKind::Bepinex6Il2cpp
                );
                let over = matches!(&s.playable, Err(now::Unplayable::NotSupported { reason }) if reason == "over-budget");
                let chain = data
                    && (over
                        || s.loader.preloader.as_deref().is_some_and(|p| {
                            p == "BepInEx.Preloader.dll" || p == "BepInEx.Unity.IL2CPP.dll"
                        }));
                let want_coexist: &[now::Coexistence] = if chain {
                    &[now::Coexistence::Chain]
                } else {
                    &[]
                };
                assert_eq!(s.coexist, want_coexist, "world {i}: {s:#?}");
                let mut v = serde_json::to_value(s).unwrap();
                v.as_object_mut().unwrap().remove("coexist");
                v
            })
            .collect();
        if got != want {
            failures.push(format!(
                "world {i} ({:?}, below {}, {:?}): 0.5.1\n{}\nexpected (0.5.0 by the rows)\n{}",
                w.flavour,
                w.below,
                w.install,
                serde_json::to_string_pretty(&got).unwrap(),
                serde_json::to_string_pretty(&want).unwrap()
            ));
            if failures.len() >= 3 {
                break;
            }
        }
        for v in &got {
            sources += 1;
            *verdicts.entry(verdict(v)).or_default() += 1;
            *managers
                .entry(v["manager"].as_str().unwrap().to_owned())
                .or_default() += 1;
            *kinds
                .entry(v["loader"]["kind"].as_str().unwrap().to_owned())
                .or_default() += 1;
        }
    }
    assert!(
        wrong_050.is_empty(),
        "verdict_050 is not 0.5.0's rule:\n{}",
        wrong_050.join("\n")
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    eprintln!(
        "mods identity: {worlds} worlds, {sources} sources, {untouched} of 0.5.0's by no row\n  verdicts {verdicts:?}\n  managers {managers:?}\n  kinds {kinds:?}\n  rows {used:?}"
    );
    // Most of 0.5.0's answers are compared as they are: the rows are the exception.
    assert!(
        untouched * 2 >= sources,
        "{untouched} of {sources} sources by no row: the rows are not the exception"
    );
    // Floors: each declared row, verdict, manager and kind is exercised, scaled to the corpus.
    let floor = |n: usize| (n as u64 * worlds / WORLDS).max(1) as usize;
    let mut low = Vec::new();
    for row in ROWS {
        // What only some file systems and platforms lay.
        if (row == Row::CaseVariant && !case_sensitive) || (row == Row::Link && !cfg!(unix)) {
            continue;
        }
        let n = used.get(&row).copied().unwrap_or(0);
        if n < floor(10) {
            low.push(format!("row {row:?}: {n}"));
        }
    }
    for v in [
        "ok",
        "loader-unsupported",
        "integration-without-mods",
        "melonloader",
        "not-supported/not-installed",
        "not-supported/unknown-key",
        "not-supported/manifest-invalid",
        "not-supported/no-doorstop",
        "not-supported/platform",
    ] {
        let n = verdicts.get(v).copied().unwrap_or(0);
        if n < floor(20) {
            low.push(format!("verdict {v}: {n}"));
        }
    }
    for m in ["r2modman", "thunderstore", "gale", "manual", "folder"] {
        let n = managers.get(m).copied().unwrap_or(0);
        if n < floor(20) {
            low.push(format!("manager {m}: {n}"));
        }
    }
    for k in [
        "bepinex5",
        "bepinex6-il2cpp",
        "bepinex6-mono",
        "bepinex6-coreclr",
        "melonloader",
    ] {
        let n = kinds.get(k).copied().unwrap_or(0);
        if n < floor(20) {
            low.push(format!("kind {k}: {n}"));
        }
    }
    assert!(low.is_empty(), "under the floor: {low:#?}");
}

/// The built-in loaders compiled in (`src/mods/builtin/*.mods.toml`, the launcher's own MPL-2.0
/// copy) are the specification's, member for member: each, and the specification's golden of the
/// same name (vendored at the pin as test data, `tests/goldens/astraplugins/`, held to its
/// `SHA256SUMS`), appended to the launcher's own built-in descriptor, give one `mods` — the same
/// canonical JSON, every loader, member and value in the same order.
#[test]
fn the_builtin_mods_are_the_specifications_fragments() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mods_of = |fragment: &str, builtin: &str| -> Value {
        let at = fragment
            .find("\n[[mods.loader]]")
            .expect("a [[mods.loader]]");
        let d = now::Descriptor::from_toml(&format!("{builtin}\n{}", &fragment[at..])).unwrap();
        assert_eq!(d.mods_refusals(), []);
        let v: Value = serde_json::from_str(d.canonical_json()).unwrap();
        v["mods"].clone()
    };
    for (name, builtin) in [
        ("unity", now::engines::BUILTIN[0]),
        ("unreal", now::engines::BUILTIN[2]),
    ] {
        let ours =
            fs::read_to_string(root.join(format!("src/mods/builtin/{name}.mods.toml"))).unwrap();
        let golden = fs::read_to_string(root.join(format!(
            "tests/goldens/astraplugins/testdata/games/descriptors/mods/builtin/{name}.mods.toml"
        )))
        .unwrap();
        let (a, b) = (mods_of(&ours, builtin), mods_of(&golden, builtin));
        assert!(a.is_object(), "{name}: {a}");
        assert_eq!(
            a, b,
            "src/mods/builtin/{name}.mods.toml is not the pinned golden's data"
        );
        // Not the golden's text: the specification's files are test data, never built in.
        assert_ne!(ours, golden, "{name}");
        assert!(
            ours.starts_with(
                "# This Source Code Form is subject to the terms of the Mozilla Public"
            )
        );
    }
}

/// The corpus is the same corpus every run.
#[test]
fn the_mods_corpus_is_deterministic() {
    let a = world(7);
    let b = world(7);
    let strip = |w: &World| -> Vec<(String, Vec<u8>)> {
        snapshot(w.tmp.path())
            .into_iter()
            .filter(|(k, _)| !k.starts_with("profiles/"))
            .collect()
    };
    let (sa, sb) = (strip(&a), strip(&b));
    assert_eq!(sa.len(), sb.len());
    for ((ka, va), (kb, vb)) in sa.iter().zip(&sb) {
        assert_eq!(ka, kb);
        if !ka.ends_with(".log") {
            assert_eq!(va, vb, "{ka}");
        }
    }
}
