// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Which program Steam launches for an app and which store categories Steam lists for it, read
//! from Steam's own `appcache/appinfo.vdf`, and where Steam lives on this machine.
//!
//! **The one Steam category table (K16)** is here: [`VAC_CATEGORY`] and [`ONLINE_CATEGORIES`].
//! Nothing else in the launcher (nor, on the daemon's side, in Astra) spells a category: the
//! daemon's online-game offer rule asks [`online_game`].
//!
//! Steam's own anti-cheat disclosure (the store's "kernel-level anti-cheat" section, 2024) is NOT
//! in `appinfo.vdf`: it is the store web API's (`IStoreBrowseService/GetItems`,
//! `extra_details.anticheat`), and a real file lacks it for a game that discloses one, so the
//! launcher cannot read it offline (docs/ENGINE-DESCRIPTORS-DEBT.md row 25).
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
/// Where Steam lists an app's store categories: one key `category_<id>` per category, valued
/// non-zero (`category_8` is "Valve Anti-Cheat enabled").
const CATEGORY_PATH: &[&str] = &["appinfo", "common", "category"];
/// A category's key below [`CATEGORY_PATH`], before its id.
const CATEGORY_KEY: &str = "category_";
/// The subtrees of an app that are kept; everything else is walked over.
const KEPT: &[&[&str]] = &[LAUNCH_PATH, CATEGORY_PATH];
/// Steam's store category "Valve Anti-Cheat enabled": the launcher's VAC refusal
/// ([`valve_anti_cheat`]). Its store name, from Steam's store API
/// (`store.steampowered.com/api/appdetails?appids=320`, Half-Life 2: Deathmatch, 2026-10-31):
/// `{"id":8,"description":"Valve Anti-Cheat enabled"}`.
pub const VAC_CATEGORY: u32 = 8;
/// The store categories that make an app an ONLINE game (marketplace plan A§4.5 "Online games
/// (H6)", merged plan §2.4), each with the code [`online_game`] answers. Never a refusal of the
/// launcher's: the daemon's offer rule (no engine-wide foundation for an online game without a
/// moderator's `games[]` row). Each id as Steam's store API names it
/// (`store.steampowered.com/api/appdetails`, 2026-10-31), and as a real `appinfo.vdf` (this
/// desk's, 683 apps) carries it under `common/category`:
/// - 36 `{"id":36,"description":"Online PvP"}` — PUBG: BATTLEGROUNDS (578080), Rust (252490);
/// - 49 `{"id":49,"description":"PvP"}` — the same two;
/// - 20 `{"id":20,"description":"MMO"}` — The Elder Scrolls Online (306130) and Rust (252490)
///   in the store API; Rust, DayZ (221100) and World of Tanks (1407200) in the real file.
pub const ONLINE_CATEGORIES: &[(u32, &str)] = &[(36, "online-pvp"), (49, "pvp"), (20, "mmo")];
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
/// entry tied to a beta branch (`config/BetaKey`), which the default branch does not run. An
/// entry whose executable is a URL (`steam://…`) is not a program and never counts.
///
/// `None` when the file is missing, unreadable or corrupt, the app is absent, or no entry fits.
/// The answer is a hint, never trusted: [`crate::detect_with`] checks it against the folder.
pub fn launch_executable(appinfo_path: &Path, appid: u32, os: &str) -> Option<String> {
    launch_executables(appinfo_path, &[appid], os).remove(&appid)
}

/// [`launch_executable`] for many apps in ONE pass over the file, which is 100+ MB on a machine
/// with a large library: a frontend listing a whole library asks once, not once per game. An app
/// with no fitting entry is simply absent from the map; a missing or corrupt file gives an empty
/// map.
pub fn launch_executables(
    appinfo_path: &Path,
    appids: &[u32],
    os: &str,
) -> std::collections::BTreeMap<u32, String> {
    let wanted: std::collections::BTreeSet<u32> = appids.iter().copied().collect();
    read_apps(appinfo_path, &wanted)
        .into_iter()
        .filter_map(|(id, app)| pick(&app, os).map(|exe| (id, exe)))
        .collect()
}

/// EVERY program Steam may start for `appid` on `os` when a person on `branches` plays it: the
/// executable of each launch entry of a game's kind (`type` `default`, absent, `none` or
/// `option<N>`) whose `config/oslist` names `os` or is empty, relative to the game's folder with
/// `/` separators, in the file's order, each once. A URL (`steam://…`) is no program. Steam
/// rewrites these from its servers when the app changes, before an update's files are
/// downloaded, so they can name a program the folder does not hold yet: what the anti-cheat gate
/// planning and Play share reads them for.
///
/// An entry tied to beta branches (`config/BetaKey`, a list separated by commas or spaces: this
/// desk's real file has `"dev_test, test_pts"` and Apex Legends' `"r5dev non_rebase_build
/// r5-260 …"`) counts only for a person on one of them ([`on_branch`]):
/// `branches` are the ones the app is on ([`crate::steam_app_branches`]), so a public test
/// branch that moved to an anti-cheat's bootstrapper refuses no one on the default branch.
/// `None` for `branches` is not known (no manifest was found): every beta entry counts then.
///
/// `None` when the file is missing, unreadable or corrupt, or the app is absent or has no
/// `config/launch`: NOT KNOWN. `Some` of an empty list is an app none of whose entries fits.
pub fn launch_programs(
    appinfo_path: &Path,
    appid: u32,
    os: &str,
    branches: Option<&[String]>,
) -> Option<Vec<String>> {
    let wanted = std::collections::BTreeSet::from([appid]);
    let app = read_apps(appinfo_path, &wanted).remove(&appid)?;
    let launch = LAUNCH_PATH
        .iter()
        .try_fold(&app, |kv, key| kv.get(key))?
        .object()?;
    let mut out: Vec<String> = Vec::new();
    for (_, e) in launch {
        let Some(entry) = game_entry(e, os) else {
            continue;
        };
        if let Some(on) = branches
            && !on_branch(&entry.betas, on)
        {
            continue;
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&entry.exe)) {
            out.push(entry.exe);
        }
    }
    Some(out)
}

/// A launch entry that starts the game on `os`.
struct GameEntry {
    /// `0` for `type` `default` (or absent), `1` for `none` or `option<N>`.
    kind_tier: u8,
    /// The beta branches it is tied to (`config/BetaKey`, split at its commas and white space,
    /// the empty ones dropped); none for an entry every branch runs.
    betas: Vec<String>,
    exe: String,
}

/// Whether a launch entry tied to `betas` (its `config/BetaKey`) is one a person on `branches`
/// ([`crate::steam_app_branches`]) may start: an entry tied to none always is; otherwise one of
/// `betas` names one of `branches`, ignoring ASCII case, the default branch being spelled
/// `public` or `default` alike (this desk's real file ties Schedule I's own program to
/// `default`). A key spelled `-<branch>` (The Backrooms Deluxe 2's `-staging`) may mean "every
/// branch but that one", so such an entry counts for every branch: the anti-cheat gate errs
/// towards refusing.
pub(crate) fn on_branch(betas: &[String], branches: &[String]) -> bool {
    let default = |b: &str| {
        b.eq_ignore_ascii_case(crate::game::STEAM_DEFAULT_BRANCH)
            || b.eq_ignore_ascii_case("default")
    };
    betas.is_empty()
        || betas.iter().any(|b| {
            b.starts_with('-')
                || branches
                    .iter()
                    .any(|on| b.eq_ignore_ascii_case(on) || (default(b) && default(on)))
        })
}

/// One launch entry when it starts the game on `os`: a game's kind (never `server`, `editor`,
/// `manual`, `vr`, `tool`, `config`, …), its `config/oslist` naming `os` or empty, and an
/// executable that is not a URL. [`launch_executable`] picks among these, [`launch_programs`]
/// lists them all.
fn game_entry(e: &Kv, os: &str) -> Option<GameEntry> {
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
    let exe = e
        .get("executable")
        .and_then(Kv::string)
        .filter(|x| !x.contains("://"))
        .and_then(normalise)?;
    Some(GameEntry {
        kind_tier,
        betas: field("BetaKey")
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|b| !b.is_empty())
            .map(str::to_owned)
            .collect(),
        exe,
    })
}

/// Whether Steam marks `appid` as protected by Valve Anti-Cheat: its `appinfo/common/category`
/// lists [`VAC_CATEGORY`] ("Valve Anti-Cheat enabled") with a value other than `0`.
///
/// `None` means NOT KNOWN, never "no": the file is missing, unreadable or corrupt, the app is not
/// in it, or its entry has no `common/category` object (Steam keeps sparse entries for apps it
/// has not fetched). `Some(false)` is an app whose categories Steam has, none of them VAC.
pub fn valve_anti_cheat(appinfo_path: &Path, appid: u32) -> Option<bool> {
    valve_anti_cheat_of(appinfo_path, &[appid]).remove(&appid)
}

/// [`valve_anti_cheat`] for many apps in ONE pass over the file, for a frontend listing a whole
/// library. An app whose flag is not known is absent from the map; a missing or corrupt file
/// gives an empty map.
pub fn valve_anti_cheat_of(
    appinfo_path: &Path,
    appids: &[u32],
) -> std::collections::BTreeMap<u32, bool> {
    let wanted: std::collections::BTreeSet<u32> = appids.iter().copied().collect();
    read_apps(appinfo_path, &wanted)
        .into_iter()
        .filter_map(|(id, app)| vac_of(&app).map(|v| (id, v)))
        .collect()
}

/// The store categories Steam lists for `appid` (`appinfo/common/category`'s `category_<id>`
/// keys valued other than `0`), ascending, each once. Read under the same bounds as
/// [`valve_anti_cheat`], and `None` exactly when it is: NOT KNOWN (no file, no app, a sparse entry
/// with no `common/category` object). `Some` of an empty list is an app Steam lists no category
/// for. A key spelled otherwise than `category_<id>` (`category_08`, `category_x`) is no
/// category, and of two keys for one id the first counts, as for VAC.
pub fn store_categories(appinfo_path: &Path, appid: u32) -> Option<Vec<u32>> {
    store_categories_of(appinfo_path, &[appid]).remove(&appid)
}

/// [`store_categories`] for many apps in ONE pass over the file. An app whose categories are not
/// known is absent from the map; a missing or corrupt file gives an empty map.
pub fn store_categories_of(
    appinfo_path: &Path,
    appids: &[u32],
) -> std::collections::BTreeMap<u32, Vec<u32>> {
    let wanted: std::collections::BTreeSet<u32> = appids.iter().copied().collect();
    read_apps(appinfo_path, &wanted)
        .into_iter()
        .filter_map(|(id, app)| categories_of(&app).map(|c| (id, c)))
        .collect()
}

/// Which of [`ONLINE_CATEGORIES`] `appid` is listed in: their codes (`online-pvp`, `pvp`,
/// `mmo`), in that table's order. `Some` of an empty list is KNOWN and none applies; `None` is not
/// known ([`store_categories`]), never "not online".
pub fn online_game(appinfo_path: &Path, appid: u32) -> Option<Vec<&'static str>> {
    store_categories(appinfo_path, appid).map(|c| online_codes(&c))
}

/// The codes of the [`ONLINE_CATEGORIES`] among `categories` (as [`store_categories`] gives
/// them), in that table's order: what [`online_game`] answers, for categories already read.
pub fn online_codes(categories: &[u32]) -> Vec<&'static str> {
    ONLINE_CATEGORIES
        .iter()
        .filter(|(id, _)| categories.contains(id))
        .map(|(_, code)| *code)
        .collect()
}

/// One app's VAC flag from its parsed KeyValues; `None` when its categories are not there.
fn vac_of(app: &Kv) -> Option<bool> {
    categories_of(app).map(|c| c.contains(&VAC_CATEGORY))
}

/// One app's store categories from its parsed KeyValues ([`store_categories`]); `None` when its
/// `common/category` object is not there.
fn categories_of(app: &Kv) -> Option<Vec<u32>> {
    let common = ["appinfo", "common"]
        .iter()
        .try_fold(app, |kv, key| kv.get(key))?;
    // No `category` object at all is NOT KNOWN: every real game lists some category, so its
    // absence says the entry is incomplete, not that VAC is off.
    let category = common.get("category")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (key, value) in category.object()? {
        let Some(id) = category_id(key) else {
            continue;
        };
        // The first key for an id is the one that counts (`Kv::get`'s, which VAC always read).
        if !seen.insert(id) {
            continue;
        }
        let listed = match value {
            Kv::String(s) => s.trim() != "0",
            Kv::Int(n) => *n != 0,
            Kv::Object(_) => true,
        };
        if listed {
            out.push(id);
        }
    }
    out.sort_unstable();
    Some(out)
}

/// The id of a key spelled `category_<id>` (ignoring ASCII case), the id in its plain decimal
/// spelling only: `category_8` is 8, `category_08` is no category.
fn category_id(key: &str) -> Option<u32> {
    let head = key.get(..CATEGORY_KEY.len())?;
    if !head.eq_ignore_ascii_case(CATEGORY_KEY) {
        return None;
    }
    let digits = &key[CATEGORY_KEY.len()..];
    let id: u32 = digits.parse().ok()?;
    (id.to_string() == digits).then_some(id)
}

/// The launch entry [`launch_executable`] picks, from one app's parsed KeyValues.
fn pick(app: &Kv, os: &str) -> Option<String> {
    let launch = LAUNCH_PATH
        .iter()
        .try_fold(app, |kv, key| kv.get(key))?
        .object()?;
    // Ranked by (tier, Steam's order). Steam's order is the numeric key; any other key keeps its
    // place in the file, after.
    let ranked = launch.iter().enumerate().filter_map(|(i, (key, e))| {
        let GameEntry {
            kind_tier,
            betas,
            exe,
        } = game_entry(e, os)?;
        let beta_tier: u8 = if betas.is_empty() { 0 } else { 2 };
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

/// A KeyValues value that was kept. Inside a kept subtree an integer is kept too (a category
/// flag may be stored as one); floats, pointers, colours and UTF-16 strings are walked over.
#[derive(Debug)]
enum Kv {
    Object(Vec<(String, Kv)>),
    String(String),
    Int(i64),
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
/// The parsed KeyValues of every app in `wanted`, read in one pass. Corruption anywhere ends the
/// pass with what was found before it; a corrupt app body is skipped.
fn read_apps(
    path: &Path,
    wanted: &std::collections::BTreeSet<u32>,
) -> std::collections::BTreeMap<u32, Kv> {
    let mut out = std::collections::BTreeMap::new();
    let Some(blobs) = read_blobs(path, wanted) else {
        return out;
    };
    let (blobs, table) = blobs;
    let keys = match &table {
        Some(t) => Keys::Table(t),
        None => Keys::Inline,
    };
    for (id, blob) in blobs {
        let mut c = Cursor { b: &blob, at: 0 };
        if let Some(obj) = object(&mut c, &keys, 0, KEPT) {
            out.insert(id, Kv::Object(obj));
        }
    }
    out
}

/// The raw bodies of the `wanted` apps, and the v29 string table, in one pass.
#[allow(clippy::type_complexity)]
fn read_blobs(
    path: &Path,
    wanted: &std::collections::BTreeSet<u32>,
) -> Option<(Vec<(u32, Vec<u8>)>, Option<Table>)> {
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
    let mut blobs = Vec::new();
    while blobs.len() < wanted.len() {
        if pos + 8 > apps_end {
            break;
        }
        let Some(id) = read_u32(&mut r) else { break };
        if id == 0 {
            break;
        }
        let Some(size) = read_u32(&mut r) else { break };
        pos += 8;
        if size < APP_HEADER || pos + u64::from(size) > apps_end {
            break;
        }
        if !wanted.contains(&id) || size - APP_HEADER > MAX_APP_BYTES {
            if r.seek_relative(i64::from(size)).is_err() {
                break;
            }
            pos += u64::from(size);
            continue;
        }
        let body = size - APP_HEADER;
        if r.seek_relative(i64::from(APP_HEADER)).is_err() {
            break;
        }
        let mut blob = vec![0u8; body as usize];
        if r.read_exact(&mut blob).is_err() {
            break;
        }
        pos += u64::from(size);
        blobs.push((id, blob));
    }
    if blobs.is_empty() {
        return Some((blobs, None));
    }
    let table = match table_at {
        Some(at) => Some(read_table(&mut r, at, len)?),
        None => None,
    };
    Some((blobs, table))
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
/// Parses one object. `paths` are the subtrees still wanted below it: an empty path keeps
/// everything, a path `[k, rest..]` keeps the child `k` and looks for `rest` below it, and an
/// object no path names is parsed (it must be walked) but not kept.
fn object<'d>(
    c: &mut Cursor<'d>,
    keys: &Keys<'d>,
    depth: usize,
    paths: &[&[&str]],
) -> Option<Vec<(String, Kv)>> {
    let mut out = Vec::new();
    let keep_all = paths.iter().any(|p| p.is_empty());
    loop {
        let ty = c.u8()?;
        if ty == T_END || ty == T_END_ALT {
            return Some(out);
        }
        let key = c.key(keys)?;
        match ty {
            T_OBJECT => {
                if depth + 1 >= MAX_DEPTH {
                    return None;
                }
                let below: Vec<&[&str]> = paths
                    .iter()
                    .filter_map(|p| match p {
                        [] => Some(&[][..]),
                        [first, rest @ ..] if key.eq_ignore_ascii_case(first.as_bytes()) => {
                            Some(rest)
                        }
                        _ => None,
                    })
                    .collect();
                let children = object(c, keys, depth + 1, &below)?;
                if !below.is_empty() {
                    out.push((text(key), Kv::Object(children)));
                }
            }
            T_STRING => {
                let s = c.cstr()?;
                if keep_all {
                    out.push((text(key), Kv::String(text(s))));
                }
            }
            T_INT32 => {
                let b = c.take(4)?;
                if keep_all {
                    let n = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    out.push((text(key), Kv::Int(i64::from(n))));
                }
            }
            T_FLOAT32 | T_POINTER | T_COLOR => {
                c.take(4)?;
            }
            T_UINT64 | T_INT64 => {
                let b = c.take(8)?;
                if keep_all {
                    let mut a = [0u8; 8];
                    a.copy_from_slice(b);
                    out.push((text(key), Kv::Int(i64::from_le_bytes(a))));
                }
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
        let keep_all: &[&[&str]] = &[&[]];
        for paths in [&[][..], keep_all, KEPT] {
            let mut c = Cursor { b: &b, at: 0 };
            assert!(object(&mut c, &Keys::Inline, 0, paths).is_none());
        }
    }
}
