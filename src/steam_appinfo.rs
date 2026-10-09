// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Which program Steam launches for an app, read from Steam's own `appcache/appinfo.vdf`, and
//! where Steam lives on this machine.
//!
//! The file is binary, large (100+ MB on a big account) and written by someone else, so it is
//! read as untrusted: buffered, skipping every other app by its size field, every length and
//! the nesting depth bounded, and any corruption answered with `None`, never a panic.
//!
//! The format, little-endian, for v28 (`0x07564428`) and v29 (`0x07564429`):
//!
//! ```text
//! header   u32 magic, u32 universe, [v29: i64 offset of the string table]
//! apps     until an appid of 0:
//!          u32 appid, u32 size (of everything after this field),
//!          u32 info_state, u32 last_updated, u64 pics_token, [20] sha1,
//!          u32 change_number, [20] sha1 of the binary data,
//!          binary KeyValues (size - 60 bytes)
//! strings  v29 only: u32 count, then count NUL-terminated strings
//! ```
//!
//! Binary KeyValues: a type byte, a key, a value. The key is a NUL-terminated string in v28 and
//! a u32 index into the string table in v29. Types: `0x00` nested (closed by `0x08`), `0x01`
//! string, `0x02` int32, `0x03` float32, `0x04` pointer, `0x05` UTF-16 string, `0x06` colour,
//! `0x07` uint64, `0x0A` int64; `0x08` (and `0x0B`) end the current object.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAGIC_V28: u32 = 0x0756_4428;
const MAGIC_V29: u32 = 0x0756_4429;

/// Bytes between an app's size field and its KeyValues.
const APP_HEADER: u32 = 4 + 4 + 8 + 20 + 4 + 20;
/// One app's KeyValues. The largest real ones are a few hundred KB.
const MAX_APP_BYTES: u32 = 16 << 20;
/// The v29 string table, all of it. A real one holds a few thousand keys.
const MAX_TABLE_BYTES: u64 = 64 << 20;
const MAX_TABLE_STRINGS: u32 = 1 << 20;
/// The only part of an app that is kept; the rest is walked over without being stored, so a
/// large or hostile app costs no memory beyond its bytes.
const LAUNCH_PATH: &[&str] = &["appinfo", "config", "launch"];
/// Nesting of KeyValues objects. Real apps nest about 8 deep.
const MAX_DEPTH: usize = 64;

const T_OBJECT: u8 = 0x00;
const T_STRING: u8 = 0x01;
const T_INT32: u8 = 0x02;
const T_FLOAT32: u8 = 0x03;
const T_POINTER: u8 = 0x04;
const T_WSTRING: u8 = 0x05;
const T_COLOR: u8 = 0x06;
const T_UINT64: u8 = 0x07;
const T_END: u8 = 0x08;
const T_INT64: u8 = 0x0A;
const T_END_ALT: u8 = 0x0B;

/// The program Steam launches for `appid` on `os` (`"windows"` or `"linux"`; on Linux a game run
/// under Proton is `"windows"`), relative to the game's folder, with `/` separators.
///
/// Of the launch entries whose `config/oslist` names `os` (or is empty), in Steam's order, the
/// first whose `type` is `default` (or absent). Real apps need two fallbacks, taken only when no
/// such entry exists: a launch option of type `option<N>` or `none` (PEAK offers only
/// `option1`–`option3` on Windows; Red Dead Redemption 2's one entry is `none`), and then an
/// entry tied to a beta branch (`config/betakey`), which the default branch does not run. An
/// entry whose executable is a URL (`steam://…`) is not a program and never counts.
///
/// `None` when the file is missing, unreadable or corrupt, the app is absent, or no entry fits.
/// The answer is a hint, never trusted: [`crate::detect_with`] checks it against the folder.
pub fn launch_executable(appinfo_path: &Path, appid: u32, os: &str) -> Option<String> {
    let app = read_app(appinfo_path, appid)?;
    let launch = LAUNCH_PATH
        .iter()
        .try_fold(&app, |kv, key| kv.get(key))?
        .object()?;
    // Ranked by (tier, Steam's order). Steam's order is the numeric key; any other key keeps its
    // place in the file, after.
    let ranked = launch.iter().enumerate().filter_map(|(i, (key, e))| {
        let kind = e
            .get("type")
            .and_then(Kv::string)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let kind_tier: u8 = if kind.is_empty() || kind == "default" {
            0
        } else if kind == "none" || kind.starts_with("option") {
            1
        } else {
            // server, editor, manual, vr, tool, config, ...: not the game.
            return None;
        };
        let config = e.get("config");
        let field = |name: &str| {
            config
                .and_then(|c| c.get(name))
                .and_then(Kv::string)
                .unwrap_or("")
                .trim()
        };
        let oslist = field("oslist");
        if !(oslist.is_empty() || oslist.split(',').any(|o| o.trim().eq_ignore_ascii_case(os))) {
            return None;
        }
        let beta_tier: u8 = if field("betakey").is_empty() { 0 } else { 2 };
        let exe = e
            .get("executable")
            .and_then(Kv::string)
            .filter(|x| !x.contains("://"))
            .and_then(normalise)?;
        let n = key.parse::<u32>().ok();
        Some(((beta_tier + kind_tier, n.is_none(), n, i), exe))
    });
    ranked.min_by_key(|(rank, _)| *rank).map(|(_, exe)| exe)
}

/// Every place Steam may live on this machine that exists, the most likely first, each folder
/// once (`~/.steam/steam` is usually a link to `~/.local/share/Steam`). Its `appcache/appinfo.vdf`
/// is the file [`launch_executable`] reads.
///
/// Linux: `~/.steam/steam`, `~/.local/share/Steam`, and the Flatpak's
/// `~/.var/app/com.valvesoftware.Steam/.local/share/Steam`. Windows: the registry's
/// `HKCU\Software\Valve\Steam\SteamPath`, else `Program Files (x86)\Steam`.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        if let Some(p) = registry_steam_path() {
            candidates.push(p);
        } else if let Some(base) = std::env::var_os("ProgramFiles(x86)") {
            candidates.push(PathBuf::from(base).join("Steam"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            candidates.push(home.join(".steam").join("steam"));
            candidates.push(home.join(".local").join("share").join("Steam"));
            candidates.push(
                home.join(".var")
                    .join("app")
                    .join("com.valvesoftware.Steam")
                    .join(".local")
                    .join("share")
                    .join("Steam"),
            );
        }
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for p in candidates {
        let Ok(real) = std::fs::canonicalize(&p) else {
            continue;
        };
        if real.is_dir() && !seen.contains(&real) {
            seen.push(real);
            out.push(p);
        }
    }
    out
}

#[cfg(windows)]
fn registry_steam_path() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let key = wide(r"Software\Valve\Steam");
    let value = wide("SteamPath");
    let mut buf = vec![0u16; 2048];
    let mut bytes = u32::try_from(buf.len() * 2).ok()?;
    // SAFETY: both names are NUL-terminated and live across the call; `buf` is writable for
    // `bytes` bytes, and RegGetValueW writes at most that many (ERROR_MORE_DATA otherwise).
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let len = (bytes as usize / 2).min(buf.len());
    let s = buf[..len].split(|&c| c == 0).next()?;
    if s.is_empty() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(s)))
}

/// `bin\x64\Game.exe` → `bin/x64/Game.exe`; a leading `./` or `/` goes, since Steam resolves the
/// executable against the install folder whatever it is written as.
fn normalise(exe: &str) -> Option<String> {
    let mut s = exe.trim().replace('\\', "/");
    loop {
        if let Some(rest) = s.strip_prefix("./") {
            s = rest.to_owned();
        } else if let Some(rest) = s.strip_prefix('/') {
            s = rest.to_owned();
        } else {
            break;
        }
    }
    (!s.is_empty()).then_some(s)
}

/// A KeyValues value that was kept. Numbers are walked over: nothing here reads one.
#[derive(Debug)]
enum Kv {
    Object(Vec<(String, Kv)>),
    String(String),
}

impl Kv {
    /// The first child named `key`; KeyValues keys are case-insensitive.
    fn get(&self, key: &str) -> Option<&Kv> {
        self.object()?
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    fn object(&self) -> Option<&[(String, Kv)]> {
        match self {
            Kv::Object(o) => Some(o),
            _ => None,
        }
    }

    fn string(&self) -> Option<&str> {
        match self {
            Kv::String(s) => Some(s),
            _ => None,
        }
    }
}

fn read_u32(r: &mut impl Read) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b))
}

fn read_i64(r: &mut impl Read) -> Option<i64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).ok()?;
    Some(i64::from_le_bytes(b))
}

/// Streams to `appid` and parses its KeyValues. The root object, holding `appinfo`.
fn read_app(path: &Path, appid: u32) -> Option<Kv> {
    let file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut r = BufReader::with_capacity(1 << 16, file);
    let magic = read_u32(&mut r)?;
    let _universe = read_u32(&mut r)?;
    let (table_at, mut pos) = match magic {
        MAGIC_V28 => (None, 8u64),
        MAGIC_V29 => {
            let at = u64::try_from(read_i64(&mut r)?).ok()?;
            if at < 16 || at > len {
                return None;
            }
            (Some(at), 16u64)
        }
        _ => return None,
    };
    // The apps end where the string table starts (v29) or at the end of the file (v28).
    let apps_end = table_at.unwrap_or(len);
    loop {
        if pos + 8 > apps_end {
            return None;
        }
        let id = read_u32(&mut r)?;
        if id == 0 {
            return None;
        }
        let size = read_u32(&mut r)?;
        pos += 8;
        if size < APP_HEADER || pos + u64::from(size) > apps_end {
            return None;
        }
        if id != appid {
            r.seek_relative(i64::from(size)).ok()?;
            pos += u64::from(size);
            continue;
        }
        let body = size - APP_HEADER;
        if body > MAX_APP_BYTES {
            return None;
        }
        r.seek_relative(i64::from(APP_HEADER)).ok()?;
        let mut blob = vec![0u8; body as usize];
        r.read_exact(&mut blob).ok()?;
        let table = match table_at {
            Some(at) => Some(read_table(&mut r, at, len)?),
            None => None,
        };
        let mut c = Cursor { b: &blob, at: 0 };
        let keys = match &table {
            Some(t) => Keys::Table(t),
            None => Keys::Inline,
        };
        return Some(Kv::Object(object(&mut c, &keys, 0, Some(LAUNCH_PATH))?));
    }
}

/// The v29 string table: its bytes, and where each string starts in them.
struct Table {
    raw: Vec<u8>,
    starts: Vec<u32>,
}

impl Table {
    fn get(&self, i: u32) -> Option<&[u8]> {
        let rest = self.raw.get(*self.starts.get(i as usize)? as usize..)?;
        Some(&rest[..rest.iter().position(|&b| b == 0)?])
    }
}

/// The v29 string table at `at`: u32 count, then count NUL-terminated strings.
fn read_table(r: &mut BufReader<File>, at: u64, len: u64) -> Option<Table> {
    let bytes = len.checked_sub(at)?;
    if !(4..=MAX_TABLE_BYTES).contains(&bytes) {
        return None;
    }
    r.seek(SeekFrom::Start(at)).ok()?;
    let count = read_u32(r)?;
    // Each string takes at least its NUL: a count past the bytes is a lie, refused before any
    // allocation is sized by it.
    if count > MAX_TABLE_STRINGS || u64::from(count) > bytes - 4 {
        return None;
    }
    let mut raw = vec![0u8; (bytes - 4) as usize];
    r.read_exact(&mut raw).ok()?;
    let mut starts = Vec::with_capacity(count as usize);
    let mut next = 0usize;
    for _ in 0..count {
        let n = raw.get(next..)?.iter().position(|&b| b == 0)?;
        starts.push(u32::try_from(next).ok()?);
        next += n + 1;
    }
    Some(Table { raw, starts })
}

enum Keys<'d> {
    /// v28: each key is a NUL-terminated string.
    Inline,
    /// v29: each key is a u32 index into the string table.
    Table(&'d Table),
}

struct Cursor<'d> {
    b: &'d [u8],
    at: usize,
}

impl<'d> Cursor<'d> {
    fn take(&mut self, n: usize) -> Option<&'d [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    /// A NUL-terminated string, without its NUL.
    fn cstr(&mut self) -> Option<&'d [u8]> {
        let rest = self.b.get(self.at..)?;
        let n = rest.iter().position(|&b| b == 0)?;
        self.at += n + 1;
        Some(&rest[..n])
    }

    /// A UTF-16 string closed by a 0x0000 unit, skipped.
    fn skip_wstr(&mut self) -> Option<()> {
        loop {
            if self.take(2)? == [0, 0] {
                return Some(());
            }
        }
    }

    fn key(&mut self, keys: &Keys<'d>) -> Option<&'d [u8]> {
        match keys {
            Keys::Inline => self.cstr(),
            Keys::Table(t) => t.get(self.u32()?),
        }
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// One object's children, up to and including its end byte. Only what lies on `path` is kept:
/// the child object named `path[0]` (with the rest of the path below it) and, once the path is
/// spent, everything. Off the path (`None`) the bytes are checked and walked over, not stored.
fn object<'d>(
    c: &mut Cursor<'d>,
    keys: &Keys<'d>,
    depth: usize,
    path: Option<&[&str]>,
) -> Option<Vec<(String, Kv)>> {
    let mut out = Vec::new();
    loop {
        let ty = c.u8()?;
        if ty == T_END || ty == T_END_ALT {
            return Some(out);
        }
        let key = c.key(keys)?;
        let keep_all = matches!(path, Some([]));
        match ty {
            T_OBJECT => {
                if depth + 1 >= MAX_DEPTH {
                    return None;
                }
                let below = match path {
                    Some([]) => Some(&[][..]),
                    Some([first, rest @ ..]) if key.eq_ignore_ascii_case(first.as_bytes()) => {
                        Some(rest)
                    }
                    _ => None,
                };
                let children = object(c, keys, depth + 1, below)?;
                if below.is_some() {
                    out.push((text(key), Kv::Object(children)));
                }
            }
            T_STRING => {
                let s = c.cstr()?;
                if keep_all {
                    out.push((text(key), Kv::String(text(s))));
                }
            }
            T_INT32 | T_FLOAT32 | T_POINTER | T_COLOR => {
                c.take(4)?;
            }
            T_UINT64 | T_INT64 => {
                c.take(8)?;
            }
            T_WSTRING => c.skip_wstr()?,
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_makes_steam_paths_relative_with_slashes() {
        assert_eq!(
            normalise(r"bin\x64\Game.exe").as_deref(),
            Some("bin/x64/Game.exe")
        );
        assert_eq!(normalise("./Game.exe").as_deref(), Some("Game.exe"));
        assert_eq!(normalise(r"\Game.exe").as_deref(), Some("Game.exe"));
        assert_eq!(normalise("  "), None);
    }

    #[test]
    fn nesting_past_the_bound_is_refused_not_a_stack_overflow() {
        // 100k nested objects with one-byte inline keys: a hostile file's cheapest attack.
        let mut b = Vec::new();
        for _ in 0..100_000 {
            b.extend_from_slice(&[T_OBJECT, b'k', 0]);
        }
        b.extend(std::iter::repeat_n(T_END, 100_001));
        for path in [None, Some(&[][..]), Some(LAUNCH_PATH)] {
            let mut c = Cursor { b: &b, at: 0 };
            assert!(object(&mut c, &Keys::Inline, 0, path).is_none());
        }
    }
}
