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

/// Options that switch Doorstop on: Doorstop 4's, then Doorstop 3's.
const ENABLE_OPTIONS: &[&str] = &["--doorstop-enabled", "--doorstop-enable"];
/// Options that name the assembly Doorstop loads: Doorstop 4's, then Doorstop 3's.
const TARGET_OPTIONS: &[&str] = &["--doorstop-target-assembly", "--doorstop-target"];
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
