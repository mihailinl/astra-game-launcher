// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Steam's `appinfo.vdf`, v28 and v29, built here byte by byte: the launch entry for an OS, and
//! a truncated or corrupt file answering `None` instead of panicking.

mod common;

use std::path::Path;

use astra_game_launcher::launch_executable;
use common::*;

const V28: u32 = 0x0756_4428;
const V29: u32 = 0x0756_4429;
const APP: u32 = 2_527_500;

/// A KeyValues value, with every type the format documents.
enum V {
    Obj(Vec<(&'static str, V)>),
    Str(&'static str),
    I32(i32),
    F32(f32),
    Ptr(u32),
    WStr(&'static str),
    Color(u32),
    U64(u64),
    I64(i64),
}

use V::*;

/// Encodes KeyValues: inline keys (v28) or indices into `table` (v29).
struct Enc<'t> {
    out: Vec<u8>,
    table: Option<&'t mut Vec<String>>,
}

impl Enc<'_> {
    fn key(&mut self, k: &str) {
        match &mut self.table {
            None => {
                self.out.extend_from_slice(k.as_bytes());
                self.out.push(0);
            }
            Some(t) => {
                let i = t.iter().position(|s| s == k).unwrap_or_else(|| {
                    t.push(k.to_owned());
                    t.len() - 1
                });
                self.out.extend_from_slice(&(i as u32).to_le_bytes());
            }
        }
    }

    fn object(&mut self, entries: &[(&str, V)]) {
        for (k, v) in entries {
            let ty: u8 = match v {
                Obj(_) => 0x00,
                Str(_) => 0x01,
                I32(_) => 0x02,
                F32(_) => 0x03,
                Ptr(_) => 0x04,
                WStr(_) => 0x05,
                Color(_) => 0x06,
                U64(_) => 0x07,
                I64(_) => 0x0A,
            };
            self.out.push(ty);
            self.key(k);
            match v {
                Obj(children) => self.object(children),
                Str(s) => {
                    self.out.extend_from_slice(s.as_bytes());
                    self.out.push(0);
                }
                I32(n) => self.out.extend_from_slice(&n.to_le_bytes()),
                F32(n) => self.out.extend_from_slice(&n.to_le_bytes()),
                Ptr(n) | Color(n) => self.out.extend_from_slice(&n.to_le_bytes()),
                WStr(s) => {
                    for u in s.encode_utf16().chain([0]) {
                        self.out.extend_from_slice(&u.to_le_bytes());
                    }
                }
                U64(n) => self.out.extend_from_slice(&n.to_le_bytes()),
                I64(n) => self.out.extend_from_slice(&n.to_le_bytes()),
            }
        }
        self.out.push(0x08);
    }
}

/// An app's KeyValues: `appinfo` with `common`, and `config/launch` when `launch` is given.
fn app(appid: u32, launch: Option<Vec<(&'static str, V)>>) -> Vec<(&'static str, V)> {
    let mut appinfo = vec![
        ("appid", I32(appid as i32)),
        (
            "common",
            Obj(vec![
                ("name", Str("A game")),
                ("type", Str("Game")),
                ("review_percentage", F32(0.93)),
                ("icon_ptr", Ptr(7)),
                ("wide_name", WStr("Игра")),
                ("tint", Color(0xff00ff)),
                ("gameid", U64(u64::MAX)),
                ("signed", I64(-5)),
            ]),
        ),
    ];
    if let Some(l) = launch {
        appinfo.push((
            "config",
            Obj(vec![("installdir", Str("Game")), ("launch", Obj(l))]),
        ));
    }
    vec![("appinfo", Obj(appinfo))]
}

fn entry(exe: &'static str, kind: Option<&'static str>, config: Vec<(&'static str, V)>) -> V {
    let mut e = vec![("executable", Str(exe))];
    if let Some(k) = kind {
        e.push(("type", Str(k)));
    }
    if !config.is_empty() {
        e.push(("config", Obj(config)));
    }
    Obj(e)
}

/// Steam's real shape for a game with a Windows and a Linux build, plus the entries that must
/// NOT be picked: another type, a beta branch, a URL, another OS.
fn launch_config() -> Vec<(&'static str, V)> {
    vec![
        (
            "0",
            entry(
                "steam://open/tools",
                Some("default"),
                vec![("oslist", Str("windows"))],
            ),
        ),
        (
            "1",
            entry(
                r"Tools\Editor.exe",
                Some("editor"),
                vec![("oslist", Str("windows"))],
            ),
        ),
        (
            "2",
            entry(
                r"bin\x64\Beta.exe",
                Some("default"),
                vec![("oslist", Str("windows")), ("BetaKey", Str("preview"))],
            ),
        ),
        (
            "3",
            entry(
                r"MiSideFull.exe",
                Some("default"),
                vec![("oslist", Str("windows")), ("osarch", Str("64"))],
            ),
        ),
        (
            "4",
            entry("Game.x86_64", None, vec![("oslist", Str("linux"))]),
        ),
        (
            "5",
            entry("Game.app", Some("default"), vec![("oslist", Str("macos"))]),
        ),
    ]
}

/// A whole file. Returns the bytes and where the app `APP` ends in them.
fn file(magic: u32, apps: &[(u32, Vec<(&'static str, V)>)]) -> (Vec<u8>, usize) {
    let v29 = magic == V29;
    let mut table: Vec<String> = Vec::new();
    let mut out = Vec::new();
    out.extend_from_slice(&magic.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // universe
    if v29 {
        out.extend_from_slice(&0i64.to_le_bytes()); // patched below
    }
    let mut app_end = 0;
    for (id, kv) in apps {
        let mut e = Enc {
            out: Vec::new(),
            table: if v29 { Some(&mut table) } else { None },
        };
        e.object(kv);
        let body = e.out;
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(60 + body.len() as u32).to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes()); // info_state
        out.extend_from_slice(&1_700_000_000u32.to_le_bytes()); // last_updated
        out.extend_from_slice(&0u64.to_le_bytes()); // pics_token
        out.extend_from_slice(&[0xAB; 20]); // sha1
        out.extend_from_slice(&42u32.to_le_bytes()); // change_number
        out.extend_from_slice(&[0xCD; 20]); // binary sha1
        out.extend_from_slice(&body);
        if *id == APP {
            app_end = out.len();
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    if v29 {
        let at = out.len() as i64;
        out[8..16].copy_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&(table.len() as u32).to_le_bytes());
        for s in &table {
            out.extend_from_slice(s.as_bytes());
            out.push(0);
        }
    }
    (out, app_end)
}

fn standard(magic: u32) -> (Vec<u8>, usize) {
    file(
        magic,
        &[
            (10, app(10, None)),
            (APP, app(APP, Some(launch_config()))),
            (
                999_999,
                app(
                    999_999,
                    Some(vec![("0", entry("Other.exe", Some("default"), vec![]))]),
                ),
            ),
        ],
    )
}

fn ask(dir: &Tmp, bytes: &[u8], appid: u32, os: &str) -> Option<String> {
    let path = dir.join("appinfo.vdf");
    std::fs::write(&path, bytes).unwrap();
    launch_executable(&path, appid, os)
}

#[test]
fn both_versions_give_each_os_its_launch_entry() {
    for magic in [V28, V29] {
        let t = Tmp::new("appinfo");
        let (bytes, _) = standard(magic);
        let v = format!("{magic:#x}");
        assert_eq!(
            ask(&t, &bytes, APP, "windows").as_deref(),
            Some("MiSideFull.exe"),
            "{v}"
        );
        assert_eq!(
            ask(&t, &bytes, APP, "linux").as_deref(),
            Some("Game.x86_64"),
            "{v}"
        );
        assert_eq!(
            ask(&t, &bytes, APP, "LINUX").as_deref(),
            Some("Game.x86_64"),
            "{v}"
        );
        assert_eq!(
            ask(&t, &bytes, APP, "macos").as_deref(),
            Some("Game.app"),
            "{v}"
        );
        // An entry with no oslist runs everywhere.
        assert_eq!(
            ask(&t, &bytes, 999_999, "linux").as_deref(),
            Some("Other.exe"),
            "{v}"
        );
        // No launch config; not in the file.
        assert_eq!(ask(&t, &bytes, 10, "windows"), None, "{v}");
        assert_eq!(ask(&t, &bytes, 12_345, "windows"), None, "{v}");
    }
}

#[test]
fn windows_separators_become_slashes() {
    let t = Tmp::new("appinfo-sep");
    let launch = vec![("0", entry(r"bin\x64\Game.exe", Some("default"), vec![]))];
    for magic in [V28, V29] {
        let (bytes, _) = file(magic, &[(APP, app(APP, Some(launch_config_of(&launch))))]);
        assert_eq!(
            ask(&t, &bytes, APP, "windows").as_deref(),
            Some("bin/x64/Game.exe")
        );
    }
}

/// `launch_config_of` rebuilds an entry list (V is not Clone).
fn launch_config_of(l: &[(&'static str, V)]) -> Vec<(&'static str, V)> {
    l.iter()
        .map(|(k, v)| {
            let V::Obj(fields) = v else { unreachable!() };
            let fields = fields
                .iter()
                .map(|(fk, fv)| match fv {
                    Str(s) => (*fk, Str(s)),
                    _ => unreachable!(),
                })
                .collect();
            (*k, Obj(fields))
        })
        .collect()
}

/// When no `default` entry fits: a launch option (PEAK offers only `option1`-`option3` on
/// Windows), then a beta branch's entry. A tool or a server never.
#[test]
fn the_fallbacks_come_in_order() {
    let t = Tmp::new("appinfo-fallback");
    // A default entry beats a launch option listed before it.
    let launch = vec![
        (
            "0",
            entry(
                "Dx12.exe",
                Some("option1"),
                vec![("oslist", Str("windows"))],
            ),
        ),
        (
            "1",
            entry(
                "Game.exe",
                Some("Default"),
                vec![("oslist", Str("windows"))],
            ),
        ),
    ];
    let (bytes, _) = file(V29, &[(APP, app(APP, Some(launch)))]);
    assert_eq!(ask(&t, &bytes, APP, "windows").as_deref(), Some("Game.exe"));

    let launch = vec![
        (
            "0",
            entry(
                "Server.exe",
                Some("server"),
                vec![("oslist", Str("windows"))],
            ),
        ),
        (
            "1",
            entry(
                "Beta.exe",
                Some("default"),
                vec![("oslist", Str("windows")), ("betakey", Str("b"))],
            ),
        ),
        (
            "2",
            entry(
                "Dx12.exe",
                Some("option1"),
                vec![("oslist", Str("windows"))],
            ),
        ),
    ];
    let (bytes, _) = file(V29, &[(APP, app(APP, Some(launch)))]);
    assert_eq!(ask(&t, &bytes, APP, "windows").as_deref(), Some("Dx12.exe"));

    let launch = vec![
        (
            "0",
            entry(
                "Server.exe",
                Some("server"),
                vec![("oslist", Str("windows"))],
            ),
        ),
        (
            "1",
            entry(
                "Beta.exe",
                Some("default"),
                vec![("oslist", Str("windows")), ("betakey", Str("b"))],
            ),
        ),
    ];
    let (bytes, _) = file(V29, &[(APP, app(APP, Some(launch)))]);
    assert_eq!(ask(&t, &bytes, APP, "windows").as_deref(), Some("Beta.exe"));

    let launch = vec![(
        "0",
        entry(
            "Server.exe",
            Some("server"),
            vec![("oslist", Str("windows"))],
        ),
    )];
    let (bytes, _) = file(V29, &[(APP, app(APP, Some(launch)))]);
    assert_eq!(ask(&t, &bytes, APP, "windows"), None);
}

/// Every strict prefix of the file: `None` whenever the app's record is cut, never a panic. A v29
/// file needs its string table, at the very end, so any cut at all is `None`.
#[test]
fn a_truncated_file_gives_none() {
    let t = Tmp::new("appinfo-cut");
    let path = t.join("appinfo.vdf");
    for magic in [V28, V29] {
        let (bytes, app_end) = standard(magic);
        for len in 0..bytes.len() {
            std::fs::write(&path, &bytes[..len]).unwrap();
            let got = launch_executable(&path, APP, "windows");
            if magic == V29 || len < app_end {
                assert_eq!(got, None, "{magic:#x} cut at {len}");
            }
        }
    }
}

/// Every byte of the file overwritten, one at a time, with values that break lengths, types and
/// indices: never a panic. And the corruptions that must be refused are.
#[test]
fn a_corrupt_file_gives_none_not_a_panic() {
    let t = Tmp::new("appinfo-bad");
    let path = t.join("appinfo.vdf");
    for magic in [V28, V29] {
        let (bytes, _) = standard(magic);
        for at in 0..bytes.len() {
            for value in [0x00, 0x01, 0x08, 0x0B, 0x7F, 0xFF] {
                let mut b = bytes.clone();
                b[at] = value;
                std::fs::write(&path, &b).unwrap();
                let _ = launch_executable(&path, APP, "windows");
            }
        }
        let refused = |b: &[u8], why: &str| {
            std::fs::write(&path, b).unwrap();
            assert_eq!(
                launch_executable(&path, APP, "windows"),
                None,
                "{magic:#x}: {why}"
            );
        };
        let mut b = bytes.clone();
        b[0] = 0x27; // v27 and anything else
        refused(&b, "an unknown magic");
        // The first app's size field, made huge, then too small for its header.
        let mut b = bytes.clone();
        let size_at = if magic == V29 { 20 } else { 12 };
        b[size_at..size_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        refused(&b, "a size past the end");
        let mut b = bytes.clone();
        b[size_at..size_at + 4].copy_from_slice(&59u32.to_le_bytes());
        refused(&b, "a size below the header");
        if magic == V29 {
            let mut b = bytes.clone();
            let past = b.len() as i64 + 1;
            b[8..16].copy_from_slice(&past.to_le_bytes());
            refused(&b, "a string table past the end");
            let mut b = bytes.clone();
            b[8..16].copy_from_slice(&(-1i64).to_le_bytes());
            refused(&b, "a negative string table offset");
            // The table's count, larger than the strings it holds.
            let at = i64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
            let mut b = bytes.clone();
            b[at..at + 4].copy_from_slice(&1_000u32.to_le_bytes());
            refused(&b, "a string count past the table");
        }
    }
}

/// An app record whose KeyValues hold an unknown type byte, or a v29 key index past the table.
#[test]
fn an_unknown_type_or_key_index_gives_none() {
    let t = Tmp::new("appinfo-type");
    let path = t.join("appinfo.vdf");
    for magic in [V28, V29] {
        let (bytes, app_end) = standard(magic);
        // The KeyValues' first byte opens `appinfo` (0x00). Find the target app's body: it ends
        // at `app_end`, and its first type byte sits right after the 68 bytes of record header.
        let body_at = find_record(&bytes, magic, APP) + 68;
        assert_eq!(bytes[body_at], 0x00);
        let mut b = bytes.clone();
        b[body_at] = 0x09;
        std::fs::write(&path, &b).unwrap();
        assert_eq!(launch_executable(&path, APP, "windows"), None, "type 0x09");
        if magic == V29 {
            let mut b = bytes.clone();
            b[body_at + 1..body_at + 5].copy_from_slice(&u32::MAX.to_le_bytes());
            std::fs::write(&path, &b).unwrap();
            assert_eq!(launch_executable(&path, APP, "windows"), None, "key index");
        }
        assert!(app_end > body_at);
    }
}

/// Where the record of `appid` starts (its appid field).
fn find_record(bytes: &[u8], magic: u32, appid: u32) -> usize {
    let mut at = if magic == V29 { 16 } else { 8 };
    loop {
        let id = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        if id == appid {
            return at;
        }
        at += 8 + size;
    }
}

#[test]
fn a_missing_file_gives_none() {
    assert_eq!(
        launch_executable(Path::new("/nonexistent/appinfo.vdf"), APP, "windows"),
        None
    );
}
