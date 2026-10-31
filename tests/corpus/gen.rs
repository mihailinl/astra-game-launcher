// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A seeded generator of game folders for canary C1: the same seed always gives the same tree.
//!
//! Each tree is a few pieces laid over one another — Unity games of every flavour, platform and
//! era, Unreal projects (shipping or not, paired or half, nested a package deeper, beside their
//! bootstrapper or a chapter launcher), Godot pairs and embedded packs, programs nothing
//! recognises, wrappers and installers, anti-cheat markers, version headers made to trip a
//! reader, shapes engine descriptors are written for, noise down past the walk's depth, now and
//! then a folder past the walk's 5,000 entries, several tied candidates, links (Unix) — and a
//! set of Steam launch hints for it: right, wrong, missing, escaping, a wrapper, too deep.
//! Pieces may collide (a file where another piece wants a folder); [`Tree::build`] keeps the first,
//! deterministically, and both detectors read whatever is on disk.
//!
//! Pieces added after C1 was first proven draw from their own streams ([`extras`], [`strays`]),
//! so every tree of the first corpus is laid exactly as before and only gains what a new stream
//! adds: a folder of 10,000 names that a listing cut BEFORE it is sorted reads differently from
//! one cut after, a game past the walk's cap with Steam's hint naming it, and the tree's Steam
//! appid ([`extras`]); the shapes only a weak or a planning-only built-in rule answers
//! ([`strays`], L2).

use std::fs;
use std::path::Path;

/// splitmix64: small, fixed, good enough to spread shapes.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    /// `pct` in 100.
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

pub enum Node {
    File(String, Vec<u8>),
    Dir(String),
    /// A link at the first path to the second, relative to the link's own folder (Unix).
    Link(String, String),
    /// `count` empty files below one folder: past the walk's cap when large.
    Many(String, usize),
    /// `count` empty files below one folder, named `z00000.dat`…: after every name that begins
    /// with a digit or a capital, so they sort after the game laid beside them.
    Fill(String, usize),
}

/// The Steam app a tree is, as the caller of planning vouches: one the fixture appinfo flags as
/// VAC, one it clears, one it does not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    Vac,
    Clear,
    Unlisted,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    pub hints: Vec<String>,
    /// Every program laid, as `/`-separated paths (some may have lost a collision).
    pub programs: Vec<String>,
    /// The tree holds a folder past the walk's 5,000 entries whose listing a cut before
    /// sorting reads differently from a cut after ([`extras`]).
    pub truncation: bool,
    pub app: Option<App>,
}

const NAMES: &[&str] = &[
    "Game",
    "MiSideFull",
    "Lethal Company",
    "Palworld",
    "Poppy_Playtime",
    "LibertyCity",
    "hl2",
    "Tool",
    "Server",
    "Editor",
    "game",
    "GAME",
    "Shooter 2",
    "a",
    "Ab",
    "x.y",
];

const PACKAGES: &[&str] = &[
    "Game",
    "WindowsNoEditor",
    "bin",
    "Pack A",
    "x64",
    "Engine",
    "Redist",
    "Tools",
    "Data",
    "Binaries",
    "Win64",
    "PlaytimeLauncher",
    "launcher",
];

const WRAPPERS: &[&str] = &[
    "Launcher.exe",
    "GameLauncher.exe",
    "UnityCrashHandler64.exe",
    "vc_redist.x64.exe",
    "EasyAntiCheat_Setup.exe",
    "PlaytimeLauncher.exe",
    "Rockstar-Games-Launcher.exe",
    "UE4PrereqSetup_x64.exe",
    "CrashReportClient.exe",
    "DXSETUP.exe",
    "start_protected_game.exe",
    "BEService_x64.exe",
];

const ANTI_CHEAT: &[(&str, bool)] = &[
    ("EasyAntiCheat", true),
    ("BattlEye", true),
    ("GameGuard", true),
    ("XIGNCODE", true),
    ("easyanticheat", true),
    ("EasyAntiCheat_EOS_Setup.exe", false),
    ("BEService.exe", false),
    ("mhypbase.dll", false),
    ("HoYoKProtect.sys", false),
    ("other.sys", false),
];

const PROGRAM_EXTS: &[&str] = &["exe", "exe", "exe", "x86_64", "x86", ""];

struct Gen {
    rng: Rng,
    nodes: Vec<Node>,
    /// Programs laid down, for hints.
    programs: Vec<String>,
}

impl Gen {
    fn file(&mut self, path: String, bytes: Vec<u8>) {
        self.nodes.push(Node::File(path, bytes));
    }

    fn program_bytes(&mut self, ext: &str) -> Vec<u8> {
        let mut b: Vec<u8> = match self.rng.below(10) {
            0 => b"PK junk".to_vec(),
            1 => Vec::new(),
            _ if ext == "exe" => b"MZ\x90\0".to_vec(),
            _ => b"\x7fELF\x02\x01".to_vec(),
        };
        let extra = match self.rng.below(20) {
            0 => 64 * 1024,
            1..=4 => 4096,
            _ => self.rng.below(512) as usize,
        };
        b.resize(b.len() + extra, b'M');
        b
    }

    fn prefix(&mut self, max: u64) -> String {
        let depth = self.rng.below(max + 1);
        let mut p = String::new();
        for _ in 0..depth {
            p.push_str(self.rng.pick(PACKAGES));
            p.push('/');
        }
        p
    }

    fn case(&mut self, s: &str) -> String {
        match self.rng.below(12) {
            0 => s.to_ascii_lowercase(),
            1 => s.to_ascii_uppercase(),
            _ => s.to_owned(),
        }
    }

    fn program(&mut self, dir: &str, stem: &str, ext: &str) -> String {
        let name = if ext.is_empty() {
            stem.to_owned()
        } else {
            format!("{stem}.{ext}")
        };
        let path = format!("{dir}{name}");
        let bytes = self.program_bytes(ext);
        self.file(path.clone(), bytes);
        self.programs.push(path.clone());
        path
    }

    fn version_header(&mut self) -> Vec<u8> {
        let mut h: Vec<u8> = (0..self.rng.below(24))
            .map(|_| *self.rng.pick(b"\x000123456789.ab\x16 "))
            .collect();
        let v = match self.rng.below(8) {
            0 => "2021.3.16f1".to_owned(),
            1 => "6000.3.1f1".to_owned(),
            2 => "5.6.0".to_owned(),
            3 => "2019.4.40f1".to_owned(),
            4 => format!(
                "{}.{}.{}{}{}",
                self.rng.below(20000),
                self.rng.below(200),
                self.rng.below(2000),
                *self.rng.pick(&['a', 'b', 'f', 'p', 'x', 'z']),
                self.rng.below(2000)
            ),
            5 => String::new(),
            6 => "UnityFS\0\0\0\0\x085.x.x\x002022.3.5f1".to_owned(),
            _ => "12.34".to_owned(),
        };
        h.extend_from_slice(v.as_bytes());
        h.push(0);
        if self.rng.chance(5) {
            h.resize(5000, b'9');
            h.extend_from_slice(b"2020.1.1f1");
        }
        h
    }

    fn unity(&mut self) {
        let dir = self.prefix(3);
        let stem = self.rng.pick(NAMES).to_string();
        let ext = self.rng.pick(PROGRAM_EXTS).to_string();
        self.program(&dir, &stem, &ext);
        match self.rng.below(6) {
            0 => {}
            1 | 2 => self.file(format!("{dir}UnityPlayer.dll"), b"MZ player".to_vec()),
            3 => self.file(format!("{dir}UnityPlayer.so"), b"\x7fELF player".to_vec()),
            4 => self.file(format!("{dir}unityplayer.DLL"), b"MZ player".to_vec()),
            _ => self.nodes.push(Node::Dir(format!("{dir}UnityPlayer.dll"))),
        }
        let data_stem = if self.rng.chance(85) {
            stem.clone()
        } else {
            self.rng.pick(NAMES).to_string()
        };
        let data_name = format!("{data_stem}_Data");
        let data = format!("{dir}{}/", self.case(&data_name));
        self.nodes.push(Node::Dir(data.clone()));
        match self.rng.below(7) {
            0 => self.file(
                format!("{data}Managed/Assembly-CSharp.dll"),
                b"MZ cs".to_vec(),
            ),
            1 => self.file(format!("{dir}GameAssembly.dll"), b"MZ ga".to_vec()),
            2 => self.file(format!("{dir}GameAssembly.so"), b"\x7fELF ga".to_vec()),
            3 => self.file(
                format!("{data}il2cpp_data/Metadata/global-metadata.dat"),
                b"x".to_vec(),
            ),
            4 => self.file(format!("{data}Managed/UnityEngine.dll"), b"MZ".to_vec()),
            5 => self.file(format!("{data}managed/assembly-csharp.dll"), b"MZ".to_vec()),
            _ => {}
        }
        if self.rng.chance(60) {
            let which = *self.rng.pick(&[
                "globalgamemanagers",
                "data.unity3d",
                "mainData",
                "GlobalGameManagers",
            ]);
            let h = self.version_header();
            self.file(format!("{data}{which}"), h);
        }
        if self.rng.chance(30) {
            let size = self.rng.below(48 * 1024) as usize;
            self.file(format!("{data}sharedassets0.assets"), vec![0; size]);
        }
    }

    fn unreal(&mut self) {
        let pkg = self.prefix(2);
        let project = self.rng.pick(NAMES).to_string();
        let proj = format!("{pkg}{}/", self.case(&project));
        let bin = format!("{proj}{}/{}/", self.case("Binaries"), self.case("Win64"));
        let shipping = self.rng.chance(60);
        if shipping {
            let prefix = if self.rng.chance(80) {
                project.clone()
            } else {
                self.rng.pick(NAMES).to_string()
            };
            let name = if self.rng.chance(85) {
                format!("{prefix}-Win64-Shipping")
            } else {
                format!("{prefix}-win64-shipping")
            };
            self.program(&bin, &name, "exe");
        }
        for _ in 0..self.rng.below(if shipping { 2 } else { 4 }) {
            let stem = self.rng.pick(NAMES).to_string();
            let path = self.program(&bin, &stem, "exe");
            if self.rng.chance(50) {
                let size = self.rng.below(32 * 1024) as usize;
                self.file(path, vec![b'M'; size.max(2)]);
            }
        }
        if self.rng.chance(15) {
            let w = self.rng.pick(WRAPPERS).to_string();
            self.file(format!("{bin}{w}"), b"MZ wrapper".to_vec());
        }
        if self.rng.chance(70) {
            let pak = *self.rng.pick(&[
                "pakchunk0-WindowsNoEditor.pak",
                "pakchunk0-Windows.pak",
                "pakchunk0-WindowsNoEditor_0_P.pak",
                "pakchunk0-LinuxNoEditor.pak",
                "Game.pak",
                "pakchunk0-WinGDK.pak",
                "notes.txt",
            ]);
            let size = self.rng.below(16 * 1024) as usize;
            let (content, paks) = (self.case("Content"), self.case("Paks"));
            self.file(format!("{proj}{content}/{paks}/{pak}"), vec![0; size]);
        }
        if self.rng.chance(30) {
            let leaf = project.clone();
            self.program(&pkg, &leaf, "exe");
        }
        if self.rng.chance(25) {
            self.file(
                format!("{pkg}Engine/Binaries/Win64/CrashReportClient.exe"),
                b"MZ crash".to_vec(),
            );
        }
        if self.rng.chance(10) {
            self.file(
                "PlaytimeLauncher/PlaytimeLauncher/Binaries/Win64/PlaytimeLauncher-Win64-Shipping.exe"
                    .into(),
                b"MZ".to_vec(),
            );
            self.file(
                "PlaytimeLauncher/PlaytimeLauncher/Content/Paks/pakchunk0-WindowsNoEditor.pak"
                    .into(),
                vec![0; 4096],
            );
        }
    }

    fn godot(&mut self) {
        let dir = self.prefix(2);
        let stem = self.rng.pick(NAMES).to_string();
        let ext = self.rng.pick(&["exe", "x86_64", "x86"]).to_string();
        let path = self.program(&dir, &stem, &ext);
        match self.rng.below(5) {
            0 | 1 => self.file(format!("{dir}{stem}.pck"), b"GDPC".to_vec()),
            2 => {
                let mut b = self.program_bytes(&ext);
                b.extend_from_slice(b"GDPC packed");
                b.extend_from_slice(&15u64.to_le_bytes());
                b.extend_from_slice(b"GDPC");
                self.file(path, b);
            }
            3 => self.file(format!("{dir}Audio.pck"), b"AKPK".to_vec()),
            _ => self.file(path, b"GDPC".to_vec()),
        }
    }

    fn unknown_programs(&mut self) {
        for _ in 0..1 + self.rng.below(3) {
            let dir = self.prefix(2);
            let stem = self.rng.pick(NAMES).to_string();
            let ext = self.rng.pick(PROGRAM_EXTS).to_string();
            self.program(&dir, &stem, &ext);
        }
        if self.rng.chance(30) {
            self.file("hl2/gameinfo.txt".into(), b"x".to_vec());
        }
    }

    fn wrapper(&mut self) {
        let dir = self.prefix(2);
        let w = self.rng.pick(WRAPPERS).to_string();
        let path = format!("{dir}{w}");
        self.file(path.clone(), b"MZ wrapper".to_vec());
        self.programs.push(path);
    }

    fn anti_cheat(&mut self) {
        let dir = self.prefix(3);
        let (name, is_dir) = *self.rng.pick(ANTI_CHEAT);
        if is_dir {
            self.file(format!("{dir}{name}/readme.txt"), b"x".to_vec());
        } else {
            self.file(format!("{dir}{name}"), b"MZ".to_vec());
        }
    }

    /// Shapes the hostile descriptors (and real ones) are written for.
    fn descriptor_shapes(&mut self) {
        let dir = self.prefix(2);
        match self.rng.below(5) {
            0 => {
                let d = format!("{dir}game/bin/win64/");
                self.program(&d, "cs2", "exe");
                self.file(format!("{d}engine2.dll"), b"MZ".to_vec());
                self.file(format!("{dir}game/core/pak01_dir.vpk"), b"vpk".to_vec());
            }
            1 => {
                let stem = self.rng.pick(NAMES).to_string();
                self.program(&dir, &stem, "exe");
                self.file(format!("{dir}UnityPlayer.dll"), b"MZ".to_vec());
            }
            2 => {
                let stem = self.rng.pick(NAMES).to_string();
                let ext = self.rng.pick(&["exe", "x86_64"]).to_string();
                self.program(&dir, &stem, &ext);
                self.file(format!("{dir}{stem}.pck"), b"GDPC".to_vec());
            }
            3 => {
                let bin = format!("{dir}Proj/Binaries/Win64/");
                self.program(&bin, "Proj-Win64-Shipping", "exe");
                self.file(format!("{bin}Game.pdb"), b"pdb".to_vec());
            }
            _ => {
                self.file(format!("{dir}Game.pdb"), b"pdb".to_vec());
                self.file(format!("{dir}engine2.dll"), b"MZ".to_vec());
            }
        }
    }

    fn noise(&mut self) {
        for _ in 0..self.rng.below(12) {
            let depth = 1 + self.rng.below(7);
            let mut p = String::new();
            for _ in 0..depth - 1 {
                p.push_str(self.rng.pick(&[
                    "a", "b", "Data", "Content", "Paks", "x", "Win64", "Binaries",
                ]));
                p.push('/');
            }
            let leaf = *self.rng.pick(&[
                "readme.txt",
                "steam_api64.dll",
                "Game.exe",
                "data.bin",
                "Assembly-CSharp.dll",
                "level0",
                "Game_Data",
                "notes.pak",
            ]);
            if self.rng.chance(20) {
                self.nodes.push(Node::Dir(format!("{p}{leaf}")));
            } else {
                let size = self.rng.below(256) as usize;
                self.file(format!("{p}{leaf}"), vec![b'n'; size]);
            }
        }
    }

    fn tied(&mut self) {
        let n = 5 + self.rng.below(3);
        for i in 0..n {
            let d = format!("Pack{i}/");
            self.program(&d, "Same", "exe");
            self.file(format!("{d}UnityPlayer.dll"), b"MZ".to_vec());
            self.file(
                format!("{d}Same_Data/Managed/Assembly-CSharp.dll"),
                b"MZ".to_vec(),
            );
        }
    }

    fn links(&mut self) {
        let target = self.programs.first().cloned();
        if let Some(t) = target {
            let name = t.rsplit('/').next().unwrap_or(&t).to_owned();
            let dir = t[..t.len() - name.len()].to_owned();
            self.nodes
                .push(Node::Link(format!("{dir}Linked.exe"), name));
        }
        self.nodes.push(Node::Link("LinkedDir".into(), ".".into()));
    }
}

/// The tree for one seed.
pub fn tree(seed: u64) -> Tree {
    let mut g = Gen {
        rng: Rng::new(seed),
        nodes: Vec::new(),
        programs: Vec::new(),
    };
    let pieces = 1 + g.rng.below(4);
    for _ in 0..pieces {
        match g.rng.below(100) {
            0..=29 => g.unity(),
            30..=54 => g.unreal(),
            55..=64 => g.godot(),
            65..=77 => g.unknown_programs(),
            78..=84 => g.wrapper(),
            85..=89 => g.anti_cheat(),
            _ => g.descriptor_shapes(),
        }
    }
    g.noise();
    if g.rng.chance(4) {
        g.tied();
    }
    if g.rng.chance(1) && g.rng.chance(40) {
        let n = 4990 + g.rng.below(30) as usize;
        g.nodes.push(Node::Many("Big".into(), n));
    }
    if cfg!(unix) && g.rng.chance(3) {
        g.links();
    }

    let mut hints = vec![String::new()];
    for p in g.programs.clone() {
        if g.rng.chance(50) {
            hints.push(p.clone());
        }
        if g.rng.chance(15) {
            hints.push(p.replace('/', "\\"));
        }
        if g.rng.chance(10) {
            hints.push(format!("./{}", p.to_ascii_uppercase()));
        }
    }
    for h in [
        "Missing.exe",
        "../outside/Game.exe",
        "/abs/Game.exe",
        "Sub/../Game.exe",
        "Launcher.exe",
        "a/b/c/d/e/Deep.exe",
        "Big/00001.dat",
        "Linked.exe",
    ] {
        if g.rng.chance(8) {
            hints.push(h.to_owned());
        }
    }
    if g.rng.chance(5) {
        g.file("a/b/c/d/e/Deep.exe".into(), b"MZ deep".to_vec());
        g.file("a/b/c/d/e/UnityPlayer.dll".into(), b"MZ".to_vec());
        g.file(
            "a/b/c/d/e/Deep_Data/Managed/Assembly-CSharp.dll".into(),
            b"MZ".to_vec(),
        );
    }
    hints.dedup();
    let mut t = Tree {
        nodes: g.nodes,
        hints,
        programs: g.programs,
        truncation: false,
        app: None,
    };
    extras(seed, &mut t);
    strays(seed, &mut t);
    t
}

/// The second stream: drawn from its own generator, laid after everything the first stream
/// laid, so it changes no tree of the first corpus but by adding to it.
fn extras(seed: u64, t: &mut Tree) {
    let mut x = Rng::new(seed.rotate_left(29) ^ 0x7E5E_C7ED_C1C1_0001);
    t.app = match x.below(100) {
        0..=14 => Some(App::Vac),
        15..=34 => Some(App::Clear),
        35..=44 => Some(App::Unlisted),
        _ => None,
    };
    if x.below(1000) >= 4 {
        return;
    }
    // `Huge/`: a game laid before 10,000 names and one after, both sorting before them. A
    // listing cut at 5,000 BEFORE it is sorted (Pass A's `listing_of`, which the hint, the
    // bootstrap and `engine_of_exe` read) loses whichever this filesystem lists late; one cut
    // after sorting would keep both. The walk itself sorts first, so it keeps both and stops
    // inside `Huge/` at its cap. The stems are drawn per tree: a filesystem that lists a folder
    // by a hash of each name (ext4) puts one name at the same place in every tree, so only
    // names that change from tree to tree land on both sides of the cut.
    t.truncation = true;
    let mut stem = || -> String {
        let tail: String = (0..3).map(|_| (b'A' + x.below(26) as u8) as char).collect();
        format!("A{tail}")
    };
    let first = stem();
    let mut second = stem();
    if second == first {
        second.push('B');
    }
    let godot = x.chance(50);
    let pair = |nodes: &mut Vec<Node>, programs: &mut Vec<String>, stem: &str| {
        let exe = format!("Huge/{stem}.exe");
        nodes.push(Node::File(exe.clone(), b"MZ\x90\0 huge".to_vec()));
        programs.push(exe);
        if godot {
            nodes.push(Node::File(format!("Huge/{stem}.pck"), b"GDPC".to_vec()));
        } else {
            nodes.push(Node::File(
                format!("Huge/{stem}_Data/Managed/Assembly-CSharp.dll"),
                b"MZ cs".to_vec(),
            ));
        }
    };
    pair(&mut t.nodes, &mut t.programs, &first);
    if !godot {
        t.nodes.push(Node::File(
            "Huge/UnityPlayer.dll".into(),
            b"MZ player".to_vec(),
        ));
    }
    t.nodes.push(Node::Fill("Huge".into(), 10_000));
    pair(&mut t.nodes, &mut t.programs, &second);
    // `Later/`: a whole Unity game the walk never reaches (its cap falls inside `Huge/`).
    t.nodes.push(Node::File(
        "Later/Game.exe".into(),
        b"MZ\x90\0 later".to_vec(),
    ));
    t.nodes.push(Node::File(
        "Later/UnityPlayer.dll".into(),
        b"MZ player".to_vec(),
    ));
    t.nodes.push(Node::File(
        "Later/Game_Data/Managed/Assembly-CSharp.dll".into(),
        b"MZ cs".to_vec(),
    ));
    t.programs.push("Later/Game.exe".into());
    for h in [
        format!("Huge/{first}.exe"),
        format!("Huge/{second}.exe"),
        format!("huge/{}.EXE", first.to_ascii_lowercase()),
        format!("HUGE/{}.exe", second.to_ascii_lowercase()),
        "Later/Game.exe".into(),
        "later/GAME.exe".into(),
    ] {
        t.hints.push(h);
    }
}

/// The third stream (L2): shapes only a weak or a planning-only built-in rule answers, which
/// neither stream above lays — a shipping program outside any `<Project>/Binaries/Win64/` (a bare
/// suffix, a wrapper's, under `Engine/`), paks with no program, a pck embedded in a program below
/// the top, a wrapper in an unshipped project's `Binaries/Win64/`, a program with no stem,
/// `Content/` spelled two ways, a pre-2017.2 Unity game below the top. Drawn from its own
/// generator, laid after everything else, so it changes no tree of the streams above but by
/// adding to it. Every program it lays is planned; some are Steam's hint.
fn strays(seed: u64, t: &mut Tree) {
    fn lay(t: &mut Tree, y: &mut Rng, path: &str, bytes: &[u8], program: bool) {
        t.nodes.push(Node::File(path.to_owned(), bytes.to_vec()));
        if program {
            t.programs.push(path.to_owned());
            if y.chance(40) {
                t.hints.push(path.to_owned());
            }
        }
    }
    let mut y = Rng::new(seed.rotate_left(43) ^ 0x57A4_5C0F_FEE0_0002);
    if !y.chance(10) {
        return;
    }
    for _ in 0..1 + y.below(3) {
        match y.below(8) {
            0 => {
                let at = *y.pick(&["", "tools/", "a/b/", "Engine/Binaries/Win64/", "Win64/"]);
                let name = *y.pick(&[
                    "Stray-Win64-Shipping.exe",
                    "-Win64-Shipping.exe",
                    "Launcher-Win64-Shipping.exe",
                    "stray-win64-shipping.EXE",
                ]);
                lay(t, &mut y, &format!("{at}{name}"), b"MZ\x90\0 stray", true);
            }
            1 => {
                let project = *y.pick(&["Cooked", "Engine", "GameLauncher", "a"]);
                let content = *y.pick(&["Content", "content", "CONTENT"]);
                for _ in 0..1 + y.below(2) {
                    let pak = *y.pick(&[
                        "Game.pak",
                        "pakchunk0-LinuxNoEditor.pak",
                        "pakchunk0-WinGDK.pak",
                        "pakchunk1-WindowsNoEditor.pak",
                    ]);
                    lay(
                        t,
                        &mut y,
                        &format!("{project}/{content}/Paks/{pak}"),
                        &[0; 16],
                        false,
                    );
                }
            }
            2 => {
                let at = *y.pick(&["bin/", "a/b/", "Win64/", ""]);
                let name = *y.pick(&["Game.exe", "Launcher.exe", "Game.x86_64"]);
                lay(
                    t,
                    &mut y,
                    &format!("{at}{name}"),
                    b"MZ\x90\0 a pck follows GDPC",
                    true,
                );
            }
            3 => {
                let wrapper = *y.pick(&[
                    "Launcher.exe",
                    "CrashReportClient.exe",
                    "UE4PrereqSetup_x64.exe",
                ]);
                let path = format!("Wrapped/Binaries/Win64/{wrapper}");
                lay(t, &mut y, &path, b"MZ wrapper", true);
                lay(
                    t,
                    &mut y,
                    "Wrapped/Content/Paks/pakchunk0-Windows.pak",
                    &[0; 8],
                    false,
                );
            }
            4 => {
                lay(t, &mut y, ".exe", b"MZ nameless", true);
                lay(t, &mut y, "UnityPlayer.dll", b"MZ player", false);
                lay(
                    t,
                    &mut y,
                    "_Data/Managed/Assembly-CSharp.dll",
                    b"MZ cs",
                    false,
                );
            }
            5 => {
                let exe = "Twice/Binaries/Win64/Twice-Win64-Shipping.exe";
                lay(t, &mut y, exe, b"MZ twice", true);
                t.nodes.push(Node::Dir("Twice/CONTENT".into()));
                lay(
                    t,
                    &mut y,
                    "Twice/content/Paks/pakchunk0-Windows.pak",
                    &[0; 8],
                    false,
                );
            }
            6 => {
                let exe = "Once/Binaries/Win64/Once-Win64-Shipping.exe";
                lay(t, &mut y, exe, b"MZ once", true);
                lay(
                    t,
                    &mut y,
                    "Once/content/Paks/pakchunk0-Windows.pak",
                    &[0; 8],
                    false,
                );
            }
            _ => {
                let name = *y.pick(&["Old.exe", "Old.x86_64", "Old"]);
                lay(t, &mut y, &format!("legacy/{name}"), b"MZ old", true);
                lay(
                    t,
                    &mut y,
                    "legacy/Old_Data/Managed/Assembly-CSharp.dll",
                    b"MZ cs",
                    false,
                );
            }
        }
    }
}

impl Tree {
    /// Lays the tree out below `root`. A node that collides with one laid before it is dropped.
    pub fn build(&self, root: &Path) {
        for n in &self.nodes {
            match n {
                Node::File(p, bytes) => {
                    let path = root.join(p);
                    if let Some(parent) = path.parent()
                        && fs::create_dir_all(parent).is_ok()
                        && !path.is_dir()
                    {
                        let _ = fs::write(&path, bytes);
                    }
                }
                Node::Dir(p) => {
                    let _ = fs::create_dir_all(root.join(p));
                }
                Node::Many(p, count) => {
                    let dir = root.join(p);
                    if fs::create_dir_all(&dir).is_ok() {
                        for i in 0..*count {
                            let _ = fs::write(dir.join(format!("{i:05}.dat")), b"");
                        }
                    }
                }
                Node::Fill(p, count) => {
                    let dir = root.join(p);
                    if fs::create_dir_all(&dir).is_ok() {
                        for i in 0..*count {
                            let _ = fs::write(dir.join(format!("z{i:05}.dat")), b"");
                        }
                    }
                }
                Node::Link(at, target) => {
                    #[cfg(unix)]
                    {
                        let path = root.join(at);
                        if let Some(parent) = path.parent()
                            && fs::create_dir_all(parent).is_ok()
                            && fs::symlink_metadata(&path).is_err()
                        {
                            let _ = std::os::unix::fs::symlink(target, &path);
                        }
                    }
                    #[cfg(not(unix))]
                    let _ = (at, target);
                }
            }
        }
    }
}
