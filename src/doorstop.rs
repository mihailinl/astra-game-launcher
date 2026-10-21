// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sharing a Unity Doorstop proxy another mod manager put beside the game.
//!
//! Doorstop (the `winhttp.dll` every BepInEx install puts beside a Unity game) is a bootloader
//! with no choice of its own: whether it loads anything, and which assembly, it reads from
//! `doorstop_config.ini`, and the command line overrides both (`--doorstop-enabled true
//! --doorstop-target-assembly <path>`). That is how r2modman and Thunderstore Mod Manager run
//! every profile through the one proxy they leave in the game folder — and it is how an
//! integration whose launch passes those options runs through that same proxy, untouched,
//! instead of refusing the game or replacing the other manager's file.
//!
//! [`stands_in`] decides when that holds, from the bytes alone: no brand, version or digest
//! list, because the question is whether THIS file honours THESE options in THIS process.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const V4_ENABLE: &str = "--doorstop-enabled";
const V3_ENABLE: &str = "--doorstop-enable";
const V4_TARGET: &str = "--doorstop-target-assembly";
/// Options that switch Doorstop on: Doorstop 4's, then Doorstop 3's.
const ENABLE_OPTIONS: &[&str] = &[V4_ENABLE, V3_ENABLE];
/// Options that name the assembly Doorstop loads: Doorstop 4's, then Doorstop 3's.
const TARGET_OPTIONS: &[&str] = &[V4_TARGET, "--doorstop-target"];
const PREFIX: &str = "--doorstop-";

/// A proxy is a small DLL (Doorstop's is ~27 KB). Anything larger is not read: it is not one.
pub(crate) const MAX_PROXY_BYTES: u64 = 8 * 1024 * 1024;

/// The Doorstop options a launch passes, when they alone decide what Doorstop does: one that
/// switches it on and one that names the target. `None` otherwise — the proxy would then load
/// whatever ITS `doorstop_config.ini` names, which is the other manager's choice, not ours.
pub(crate) fn launch_options(args: &[String]) -> Option<Vec<&str>> {
    let opts: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| a.starts_with(PREFIX))
        .collect();
    let has = |set: &[&str]| opts.iter().any(|o| set.contains(o));
    (has(ENABLE_OPTIONS) && has(TARGET_OPTIONS)).then_some(opts)
}

/// Whether `bin` carries `option` as a string of its own — NUL (or the file's start) on both
/// sides — as UTF-16LE (Windows Doorstop compares wide strings) or as UTF-8. Both bounds matter:
/// `--doorstop-target` (Doorstop 3) is a prefix of `--doorstop-target-assembly` (Doorstop 4),
/// which does not understand the Doorstop 3 spelling, and a usage text that merely QUOTES an
/// option (`"Usage: --doorstop-enabled"`) is not a parser comparing against it.
pub(crate) fn understands(bin: &[u8], option: &str) -> bool {
    let wide: Vec<u8> = option.encode_utf16().flat_map(u16::to_le_bytes).collect();
    bounded(bin, &wide, &[0, 0]) || bounded(bin, option.as_bytes(), &[0])
}

/// `needle` somewhere in `hay` with `nul` right after it and right before it (or nothing before).
fn bounded(hay: &[u8], needle: &[u8], nul: &[u8]) -> bool {
    let n = needle.len();
    (0..hay.len().saturating_sub(n + nul.len() - 1)).any(|i| {
        hay[i..].starts_with(needle)
            && hay[i + n..].starts_with(nul)
            && (i == 0 || hay[..i].ends_with(nul))
    })
}

/// The `Machine` field of a PE image's COFF header (`0x8664` for x64), or `None` when `bytes`
/// is not a PE image.
pub(crate) fn pe_machine(bytes: &[u8]) -> Option<u16> {
    if bytes.get(..2)? != b"MZ" {
        return None;
    }
    let off = u32::from_le_bytes(bytes.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if bytes.get(off..off.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let m = bytes.get(off + 4..off + 6)?;
    Some(u16::from_le_bytes([m[0], m[1]]))
}

/// [`pe_machine`] of a file on disk, reading only its headers (a game's exe may be 100+ MB).
pub(crate) fn pe_machine_of_file(path: &Path) -> Option<u16> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 0x40];
    f.read_exact(&mut head).ok()?;
    if &head[..2] != b"MZ" {
        return None;
    }
    let off = u32::from_le_bytes(head[0x3C..0x40].try_into().ok()?) as u64;
    let mut pe = [0u8; 6];
    f.seek(SeekFrom::Start(off)).ok()?;
    f.read_exact(&mut pe).ok()?;
    (&pe[..4] == b"PE\0\0").then(|| u16::from_le_bytes([pe[4], pe[5]]))
}

/// Whether `bin` is a proxy the game's process can load (`exe_machine`) and that honours every
/// Doorstop option of `args`.
pub(crate) fn serves(bin: &[u8], exe_machine: Option<u16>, args: &[String]) -> bool {
    let Some(opts) = launch_options(args) else {
        return false;
    };
    exe_machine.is_some()
        && pe_machine(bin) == exe_machine
        && opts.iter().all(|o| understands(bin, o))
}

/// Whether `theirs`, a proxy already beside the game, does everything `ours` was shipped to do
/// for a launch with `args`: both are Doorstop builds that honour every Doorstop option the
/// launch passes, and theirs loads into the game's process. Ours must qualify too — a proxy of
/// another kind under the same name (a ReShade `dxgi.dll`) is never stood in for by a Doorstop.
pub(crate) fn stands_in(
    ours: &[u8],
    theirs: &[u8],
    exe_machine: Option<u16>,
    args: &[String],
) -> bool {
    let ours_is_doorstop =
        launch_options(args).is_some_and(|opts| opts.iter().all(|o| understands(ours, o)));
    ours_is_doorstop && serves(theirs, exe_machine, args)
}

/// Reads a proxy beside the game, `None` when it is not a plain file (a link is never followed),
/// cannot be read, or is larger than any proxy.
pub(crate) fn read_proxy(path: &Path) -> Option<Vec<u8>> {
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let f = File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(MAX_PROXY_BYTES + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= MAX_PROXY_BYTES).then_some(buf)
}

/// Which Doorstop a proxy is, from the options it understands; `None`: not a Doorstop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Generation {
    V3,
    V4,
}

pub(crate) fn generation(bin: &[u8]) -> Option<Generation> {
    if understands(bin, V4_ENABLE) {
        Some(Generation::V4)
    } else if understands(bin, V3_ENABLE) {
        Some(Generation::V3)
    } else {
        None
    }
}

/// The settings file Doorstop reads from the game's working folder.
pub(crate) const CONFIG_NAME: &str = "doorstop_config.ini";
/// The target both generations load when the file is there and names none.
const DEFAULT_TARGET: &str = "Doorstop.dll";
/// More than any `doorstop_config.ini`; only this much is read.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// The text of the `doorstop_config.ini` at `path` (links followed: Doorstop follows them), as
/// Windows reads it: UTF-16LE after its BOM, otherwise UTF-8 (a BOM dropped later, invalid bytes
/// replaced). `None` when there is no file to read.
pub(crate) fn read_config(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_CONFIG_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    if let Some(wide) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = wide
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// What a Doorstop of `generation` loads on a start that passes it no options — a plain Play in
/// Steam — given `ini`, the `doorstop_config.ini` beside it: the target assembly as that file
/// writes it, or `None` when the file switches Doorstop off or names nothing.
///
/// Doorstop's own rules, in both generations (`[General]` in 4, `[UnityDoorstop]` in 3): a key
/// missing from the file takes the default Doorstop hands `GetPrivateProfileString`, `enabled`
/// = `true` and the target `Doorstop.dll`; a present `enabled` switches it on only when it is
/// `true` in any letter case (anything but `true`/`false` keeps the built-in default, off); an
/// empty target names nothing. No file at all: off — the caller's case, not this one.
pub(crate) fn plain_start_target(ini: &str, generation: Generation) -> Option<String> {
    let (section, target_key) = match generation {
        Generation::V4 => ("General", "target_assembly"),
        Generation::V3 => ("UnityDoorstop", "targetAssembly"),
    };
    let enabled =
        crate::ini::get(ini, section, "enabled").is_none_or(|v| v.eq_ignore_ascii_case("true"));
    let target =
        crate::ini::get(ini, section, target_key).unwrap_or_else(|| DEFAULT_TARGET.to_owned());
    (enabled && !target.is_empty()).then_some(target)
}

/// Doorstop 4 options a launch adds to the manifest's own, so that a `doorstop_config.ini` that
/// is not ours — a shared loader's, or one kept beside our proxy — cannot change Astra's session.
///
/// Always: no Mono debugger (`debug_suspend` holds the game until a debugger attaches). Both are
/// Doorstop's defaults, booleans every Doorstop 4 build reads.
///
/// Only when `foreign_config` (a `doorstop_config.ini` that is not our own unchanged one is where
/// the game starts): the game's own `boot.config`, named relative to the working folder, which is
/// the very path Doorstop compares against, so it redirects that file to itself. Not always: on
/// 4.3+ naming an existing file switches Doorstop's file-open hooks on for the whole session, and
/// with our own config nothing overrides `boot.config` in the first place.
///
/// A Doorstop that does not know an option ignores it, and reads no such setting from its file
/// either (`boot-config-override` came to both in 4.3), so none of these is asked of a shared
/// loader: [`serves`] asks only for the manifest's options.
///
/// `dll_search_path_override` is left to the file: it has no neutral value (pointing it at the
/// game's own `Managed` folder is not one, as Doorstop then loads each assembly from disk instead
/// of the bytes the game hands Mono), and a person who set it usually has a game that needs it.
///
/// Only for a Doorstop 4 launch (one that passes `--doorstop-enabled` and
/// `--doorstop-target-assembly`), and never an option the manifest passes itself. Each value is
/// one argv element whatever it holds: spawning passes argv, never a shell string.
pub(crate) fn session_options(
    args: &[String],
    exe_file_name: &str,
    foreign_config: bool,
) -> Vec<String> {
    let has = |o: &str| args.iter().any(|a| a.eq_ignore_ascii_case(o));
    if !(has(V4_ENABLE) && has(V4_TARGET)) {
        return Vec::new();
    }
    let mut pins = vec![
        ("--doorstop-mono-debug-enabled", "false".to_owned()),
        ("--doorstop-mono-debug-suspend", "false".to_owned()),
    ];
    // Unity reads `<exe name>_Data/boot.config`. A name holding `${` would read as a
    // placeholder: no pin then.
    let stem = Path::new(exe_file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if foreign_config && !stem.is_empty() && !stem.contains("${") {
        pins.push((
            "--doorstop-boot-config-override",
            format!("{stem}_Data/boot.config"),
        ));
    }
    pins.into_iter()
        .filter(|(o, _)| !has(o))
        .flat_map(|(o, v)| [o.to_owned(), v])
        .collect()
}

/// Names a Doorstop build goes by beside a game (its proxy forwards to the real system DLL of the
/// same name). Probed by bytes, so another loader under one of them (ReShade's `dxgi.dll`) is
/// never taken for a Doorstop.
pub(crate) const PROXY_NAMES: &[&str] = &[
    "winhttp.dll",
    "version.dll",
    "winmm.dll",
    "dxgi.dll",
    "d3d11.dll",
    "dinput8.dll",
];

#[cfg(test)]
mod tests {
    use super::*;

    const X64: u16 = 0x8664;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn v4_args() -> Vec<String> {
        args(&[
            "--doorstop-enabled",
            "true",
            "--doorstop-target-assembly",
            "${profile}/BepInEx/core/BepInEx.Preloader.dll",
        ])
    }

    /// A PE image header for `machine` followed by `options` as NUL-terminated wide strings.
    fn proxy(machine: u16, options: &[&str]) -> Vec<u8> {
        let mut b = vec![0u8; 0x80];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        b[0x40..0x44].copy_from_slice(b"PE\0\0");
        b[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
        for o in options {
            b.extend(o.encode_utf16().flat_map(u16::to_le_bytes));
            b.extend([0, 0]);
        }
        b
    }

    const V4: &[&str] = &["--doorstop-enabled", "--doorstop-target-assembly"];
    const V3: &[&str] = &["--doorstop-enable", "--doorstop-target"];

    #[test]
    fn a_launch_must_choose_both_enable_and_target() {
        assert!(launch_options(&v4_args()).is_some());
        assert!(launch_options(&args(&["--doorstop-enabled", "true"])).is_none());
        assert!(launch_options(&args(&["--doorstop-target-assembly", "x"])).is_none());
        assert!(launch_options(&args(&["-batchmode"])).is_none());
    }

    #[test]
    fn a_v4_proxy_stands_in_for_a_v4_proxy() {
        let ours = proxy(X64, V4);
        let theirs = proxy(X64, &[V4[0], "--doorstop-bypass-vulkan", V4[1]]);
        assert!(stands_in(&ours, &theirs, Some(X64), &v4_args()));
    }

    #[test]
    fn a_v3_proxy_does_not_understand_v4_options_nor_the_reverse() {
        let v3 = proxy(X64, V3);
        let v4 = proxy(X64, V4);
        assert!(!stands_in(&v4, &v3, Some(X64), &v4_args()));
        let v3_args = args(&["--doorstop-enable", "true", "--doorstop-target", "x.dll"]);
        // `--doorstop-target` is a prefix of `--doorstop-target-assembly`: the terminator decides.
        assert!(!stands_in(&v3, &v4, Some(X64), &v3_args));
        assert!(stands_in(&v3, &v3, Some(X64), &v3_args));
    }

    #[test]
    fn a_proxy_for_another_architecture_is_not_loaded_by_the_game() {
        let ours = proxy(X64, V4);
        let x86 = proxy(0x014C, V4);
        assert!(!stands_in(&ours, &x86, Some(X64), &v4_args()));
        assert!(
            !stands_in(&ours, &ours, None, &v4_args()),
            "an exe of unknown kind"
        );
    }

    #[test]
    fn something_that_is_not_doorstop_never_stands_in_nor_is_stood_in_for() {
        let doorstop = proxy(X64, V4);
        let other = proxy(X64, &["--some-other-loader"]);
        assert!(!stands_in(&doorstop, &other, Some(X64), &v4_args()));
        assert!(!stands_in(&other, &doorstop, Some(X64), &v4_args()));
        assert!(!stands_in(&doorstop, b"MZ not a pe", Some(X64), &v4_args()));
    }

    #[test]
    fn an_option_quoted_inside_a_longer_string_is_not_understood() {
        let doorstop = proxy(X64, V4);
        let quoting = proxy(
            X64,
            &["Usage: --doorstop-enabled", "xx--doorstop-target-assembly"],
        );
        assert!(!stands_in(&doorstop, &quoting, Some(X64), &v4_args()));
        // At the very start of the bytes, with nothing before it, it is its own string.
        let mut at_start: Vec<u8> = V4[0].encode_utf16().flat_map(u16::to_le_bytes).collect();
        at_start.extend([0, 0]);
        assert!(understands(&at_start, V4[0]));
    }

    #[test]
    fn utf8_options_count_too() {
        let mut theirs = proxy(X64, &[]);
        for o in V4 {
            theirs.extend(o.as_bytes());
            theirs.push(0);
        }
        assert!(stands_in(&proxy(X64, V4), &theirs, Some(X64), &v4_args()));
    }

    #[test]
    fn a_doorstop_4_launch_pins_what_a_config_not_ours_could_change() {
        let debugger = args(&[
            "--doorstop-mono-debug-enabled",
            "false",
            "--doorstop-mono-debug-suspend",
            "false",
        ]);
        // Our own config beside the game: nothing overrides boot.config, so no file hooks.
        assert_eq!(
            session_options(&v4_args(), "Some Game.exe", false),
            debugger
        );
        // Someone else's: their boot.config override is pointed back at the game's own, named
        // as Doorstop compares it (working folder + program name + `_Data`), one argv element.
        let mut with_boot = debugger.clone();
        with_boot.extend(args(&[
            "--doorstop-boot-config-override",
            "Some Game_Data/boot.config",
        ]));
        assert_eq!(
            session_options(&v4_args(), "Some Game.exe", true),
            with_boot
        );
        // Unity on Linux names the data folder after the program without its extension too.
        assert!(
            session_options(&v4_args(), "Game.x86_64", true)
                .contains(&"Game_Data/boot.config".to_owned())
        );
        // The manifest's own choice wins, in any letter case (Doorstop compares with lstrcmpi).
        let mut own = v4_args();
        own.extend(args(&["--Doorstop-Mono-Debug-Enabled", "true"]));
        let pins = session_options(&own, "g.exe", true);
        assert!(
            !pins
                .iter()
                .any(|a| a.eq_ignore_ascii_case("--doorstop-mono-debug-enabled")),
            "{pins:?}"
        );
        assert!(pins.iter().any(|a| a == "--doorstop-mono-debug-suspend"));
        // Not a Doorstop 4 launch: nothing.
        let v3 = args(&["--doorstop-enable", "true", "--doorstop-target", "x.dll"]);
        assert!(session_options(&v3, "g.exe", true).is_empty());
        assert!(session_options(&args(&["-batchmode"]), "g.exe", true).is_empty());
        // A program name that would read as a placeholder gets no path.
        assert!(
            !session_options(&v4_args(), "${x}.exe", true)
                .iter()
                .any(|a| a.contains("boot"))
        );
    }

    #[test]
    fn what_a_plain_start_loads_follows_the_generation() {
        assert_eq!(generation(&proxy(X64, V4)), Some(Generation::V4));
        assert_eq!(generation(&proxy(X64, V3)), Some(Generation::V3));
        assert_eq!(generation(&proxy(X64, &["--other-loader"])), None);
        let v3_ini = "[UnityDoorstop]\nenabled=true\ntargetAssembly=a.dll\n";
        assert_eq!(
            plain_start_target(v3_ini, Generation::V3).as_deref(),
            Some("a.dll")
        );
        assert_eq!(
            plain_start_target(v3_ini, Generation::V4).as_deref(),
            Some("Doorstop.dll"),
            "no [General]: Doorstop 4's defaults for missing keys"
        );
        assert_eq!(
            plain_start_target(
                "[General]\nenabled=false\ntarget_assembly=a.dll\n",
                Generation::V4
            ),
            None
        );
    }

    #[test]
    fn the_machine_is_read_from_the_headers_of_a_file() {
        let dir = std::env::temp_dir().join(format!("agl-doorstop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("game.exe");
        let mut exe = proxy(X64, &[]);
        exe.resize(4096, 0);
        std::fs::write(&p, &exe).unwrap();
        assert_eq!(pe_machine_of_file(&p), Some(X64));
        std::fs::write(&p, b"\x7fELF").unwrap();
        assert_eq!(pe_machine_of_file(&p), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
