// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Wine DLL override in a Proton prefix's `user.reg`, under
//! `[Software\\Wine\\AppDefaults\\<exe>\\DllOverrides]`. Only that section is touched, the rest
//! of the file keeps its bytes, the write is atomic, and what was there before is recorded so
//! uninstall can put it back.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{LauncherError, Result, codes};
use crate::ledger::ProtonRecord;
use crate::paths::{check_regular_or_absent, write_atomic};

/// The registry key, unescaped.
pub(crate) fn section_for(exe_file: &str) -> String {
    format!("Software\\Wine\\AppDefaults\\{exe_file}\\DllOverrides")
}

/// Wine's escaping for a key name inside `[...]`.
fn escape_key(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '[' | ']' => {
                out.push('\\');
                out.push(ch);
            }
            c if (c as u32) < 32 || (c as u32) > 127 => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\x{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Reads an escaped string up to the unescaped `stop`. Returns it and the byte index just after
/// `stop`.
fn unescape_until(s: &str, stop: char) -> Option<(String, usize)> {
    let mut units: Vec<u16> = Vec::new();
    let mut chars = s.char_indices().peekable();
    let push = |units: &mut Vec<u16>, c: char| {
        let mut buf = [0u16; 2];
        units.extend_from_slice(c.encode_utf16(&mut buf));
    };
    while let Some((i, c)) = chars.next() {
        if c == stop {
            return Some((String::from_utf16_lossy(&units), i + c.len_utf8()));
        }
        if c != '\\' {
            push(&mut units, c);
            continue;
        }
        let (_, e) = chars.next()?;
        match e {
            'x' => {
                let mut v: u32 = 0;
                let mut n = 0;
                while n < 4 {
                    match chars.peek() {
                        Some((_, h)) if h.is_ascii_hexdigit() => {
                            v = v * 16 + h.to_digit(16).unwrap_or(0);
                            chars.next();
                            n += 1;
                        }
                        _ => break,
                    }
                }
                units.push(v as u16);
            }
            '0'..='7' => {
                let mut v: u32 = e.to_digit(8).unwrap_or(0);
                let mut n = 1;
                while n < 3 {
                    match chars.peek() {
                        Some((_, d)) if ('0'..='7').contains(d) => {
                            v = v * 8 + d.to_digit(8).unwrap_or(0);
                            chars.next();
                            n += 1;
                        }
                        _ => break,
                    }
                }
                units.push(v as u16);
            }
            'a' => units.push(7),
            'b' => units.push(8),
            't' => units.push(9),
            'n' => units.push(10),
            'v' => units.push(11),
            'f' => units.push(12),
            'r' => units.push(13),
            'e' => units.push(27),
            other => push(&mut units, other),
        }
    }
    None
}

fn bare(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

fn header_key(line: &str) -> Option<String> {
    let rest = bare(line).strip_prefix('[')?;
    unescape_until(rest, ']').map(|(k, _)| k)
}

fn same_key(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// `(header index, end index exclusive)` of a section.
fn find_section(lines: &[String], key: &str) -> Option<(usize, usize)> {
    let start = lines
        .iter()
        .position(|l| header_key(l).is_some_and(|k| same_key(&k, key)))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with('['))
        .map_or(lines.len(), |p| start + 1 + p);
    Some((start, end))
}

/// The value name of a `"name"=…` line, and the text after `=`.
fn value_line(line: &str) -> Option<(String, String)> {
    let rest = bare(line).strip_prefix('"')?;
    let (name, used) = unescape_until(rest, '"')?;
    let after = rest[used..].strip_prefix('=')?;
    Some((name, after.to_owned()))
}

fn find_value(lines: &[String], start: usize, end: usize, dll: &str) -> Option<usize> {
    (start + 1..end).find(|&i| value_line(&lines[i]).is_some_and(|(n, _)| same_key(&n, dll)))
}

fn last_non_blank(lines: &[String], start: usize, end: usize) -> usize {
    (start..end)
        .rev()
        .find(|&i| !bare(&lines[i]).trim().is_empty())
        .unwrap_or(start)
}

fn split(text: &str) -> (Vec<String>, &'static str) {
    let cr = if text.contains("\r\n") { "\r" } else { "" };
    (text.split('\n').map(str::to_owned).collect(), cr)
}

pub(crate) struct RegEdit {
    pub text: String,
    pub changed: bool,
    /// The value line existed before (with any value).
    pub line_present: bool,
    pub previous: Option<String>,
    pub section_created: bool,
}

/// Sets `"dll"="value"` in the section `key`.
pub(crate) fn apply(text: &str, key: &str, dll: &str, value: &str, now_secs: u64) -> RegEdit {
    let (mut lines, cr) = split(text);
    let ours_text = format!("\"{value}\"");
    let ours_line = format!("\"{dll}\"={ours_text}{cr}");
    if let Some((start, end)) = find_section(&lines, key) {
        if let Some(i) = find_value(&lines, start, end, dll) {
            let (_, prev) = value_line(&lines[i]).unwrap_or_default();
            let changed = prev != ours_text;
            if changed {
                lines[i] = ours_line;
            }
            return RegEdit {
                text: lines.join("\n"),
                changed,
                line_present: true,
                previous: Some(prev),
                section_created: false,
            };
        }
        let at = last_non_blank(&lines, start, end) + 1;
        lines.insert(at, ours_line);
        return RegEdit {
            text: lines.join("\n"),
            changed: true,
            line_present: false,
            previous: None,
            section_created: false,
        };
    }
    let header = format!("[{}] {now_secs}{cr}", escape_key(key));
    if lines.len() == 1 && lines[0].is_empty() {
        lines.clear();
    } else if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if !lines.is_empty() {
        lines.push(cr.to_owned());
    }
    lines.push(header);
    lines.push(ours_line);
    lines.push(String::new());
    RegEdit {
        text: lines.join("\n"),
        changed: true,
        line_present: false,
        previous: None,
        section_created: true,
    }
}

/// Puts back one record's line, when the value there is still ours. Returns whether a line
/// changed. The section header is left to [`drop_owned_empty_sections`].
fn restore_line(lines: &mut Vec<String>, cr: &str, rec: &ProtonRecord) -> bool {
    let Some((start, end)) = find_section(lines, &rec.section) else {
        return false;
    };
    let ours_text = format!("\"{}\"", rec.value);
    let Some(i) = find_value(lines, start, end, &rec.dll) else {
        return false;
    };
    let (_, current) = value_line(&lines[i]).unwrap_or_default();
    if current != ours_text {
        return false;
    }
    match &rec.previous {
        Some(prev) => {
            let new = format!("\"{}\"={prev}{cr}", rec.dll);
            if new == lines[i] {
                return false;
            }
            lines[i] = new;
        }
        None => {
            lines.remove(i);
        }
    }
    true
}

/// Removes every section ANY of `records` created that holds no value any more, whichever
/// record created it and in whatever order the lines were removed: a header is never orphaned
/// because the line that created it went first.
fn drop_owned_empty_sections(lines: &mut Vec<String>, records: &[&ProtonRecord]) -> bool {
    let mut changed = false;
    let mut done: Vec<String> = Vec::new();
    for rec in records.iter().filter(|r| r.section_created) {
        if done.iter().any(|d| same_key(d, &rec.section)) {
            continue;
        }
        done.push(rec.section.clone());
        let Some((start, end)) = find_section(lines, &rec.section) else {
            continue;
        };
        let has_values = (start + 1..end).any(|i| {
            let l = bare(&lines[i]);
            l.starts_with('"') || l.starts_with('@')
        });
        if !has_values {
            let stop = last_non_blank(lines, start, end) + 1;
            let from = if start > 0 && bare(&lines[start - 1]).trim().is_empty() {
                start - 1
            } else {
                start
            };
            lines.drain(from..stop);
            changed = true;
        }
    }
    changed
}

/// Puts back what [`apply`] replaced for every record, newest first, then drops the sections
/// they created that are empty again. Returns the new text when anything changed.
pub(crate) fn restore_all(text: &str, records: &[&ProtonRecord]) -> Option<String> {
    let (mut lines, cr) = split(text);
    let mut changed = false;
    for rec in records.iter().rev() {
        changed |= restore_line(&mut lines, cr, rec);
    }
    changed |= drop_owned_empty_sections(&mut lines, records);
    changed.then(|| lines.join("\n"))
}

/// [`restore_all`] for one record.
#[cfg(test)]
pub(crate) fn restore(text: &str, rec: &ProtonRecord) -> Option<String> {
    restore_all(text, &[rec])
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn missing(prefix: &Path) -> LauncherError {
    LauncherError::new(codes::PROTON_PREFIX_MISSING).with("prefix", prefix.display().to_string())
}

/// Two spellings of one prefix (`~/.steam/steam/...` and `~/.local/share/Steam/...` through
/// Steam's link) are one prefix: compared by canonical path, or as written when either is gone.
pub(crate) fn same_prefix(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
thread_local! {
    /// Where the unit tests put Wine's server directories instead of `/tmp/.wine-<uid>`.
    static TEST_SERVER_ROOT: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Wine's server directory root for this user: `/tmp/.wine-<uid>`.
#[cfg(target_os = "linux")]
fn server_root() -> Option<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt;
    #[cfg(test)]
    if let Some(r) = TEST_SERVER_ROOT.with(|r| r.borrow().clone()) {
        return Some(r);
    }
    // `/proc/self` belongs to this process's user: the uid without a libc call.
    let uid = fs::metadata("/proc/self").ok()?.uid();
    Some(std::path::PathBuf::from(format!("/tmp/.wine-{uid}")))
}

/// Whether a wineserver serves `prefix` right now. Wine names its socket after the prefix
/// folder's device and inode, `<root>/server-<dev hex>-<ino hex>/socket`; the server is alive
/// when that socket ACCEPTS a connection, so a directory a crashed server left behind never
/// blocks anything.
#[cfg(target_os = "linux")]
pub(crate) fn wineserver_alive(prefix: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Some(root), Ok(meta)) = (server_root(), fs::metadata(prefix)) else {
        return false;
    };
    let socket = root
        .join(format!("server-{:x}-{:x}", meta.dev(), meta.ino()))
        .join("socket");
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn wineserver_alive(_prefix: &Path) -> bool {
    false
}

/// Refuses while Wine runs in `prefix`: wineserver keeps the registry in memory and writes it
/// back over `user.reg` when it exits, so an edit made now would be lost (or would be the
/// version lost).
pub(crate) fn refuse_if_wineserver(prefix: &Path) -> Result<()> {
    if wineserver_alive(prefix) {
        return Err(LauncherError::new(codes::GAME_RUNNING)
            .with("reason", "wineserver")
            .with("prefix", prefix.display().to_string()));
    }
    Ok(())
}

/// The value text a `"dll"=` line holds in `section`, if any.
fn current_value(text: &str, section: &str, dll: &str) -> Option<String> {
    let (lines, _) = split(text);
    let (start, end) = find_section(&lines, section)?;
    let i = find_value(&lines, start, end, dll)?;
    value_line(&lines[i]).map(|(_, v)| v)
}

/// Applies every override in `prefix/user.reg`, in the order given, with one atomic write.
///
/// The records come back as: every record of `existing` this call does not replace (another
/// prefix, another section, a DLL no longer asked for — all still to be restored on uninstall),
/// then one record per override in order. A record is replaced only by one for the same DLL in
/// the same section of the same prefix (canonical path), and keeps its first "before". A value
/// we wrote is never recorded as the user's `previous`.
///
/// `persist` receives the records BEFORE `user.reg` is touched (it writes the ledger), so a crash
/// in between leaves a ledger that knows about a change not yet made, which restoring skips,
/// never a change the ledger does not know about. After writing, `user.reg` is read again and
/// every override must be there. Refused while a wineserver serves the prefix.
pub(crate) fn apply_prefix(
    prefix: &Path,
    exe_file: &str,
    overrides: &[(String, String)],
    existing: &[ProtonRecord],
    persist: impl FnOnce(&[ProtonRecord]) -> Result<()>,
) -> Result<Vec<ProtonRecord>> {
    let reg = prefix.join("user.reg");
    if !fs::symlink_metadata(prefix).is_ok_and(|m| m.is_dir()) {
        return Err(missing(prefix));
    }
    refuse_if_wineserver(prefix)?;
    check_regular_or_absent(&reg)?;
    let read = || match fs::read(&reg) {
        Ok(b) => String::from_utf8(b).map_err(|_| LauncherError::io("user.reg is not UTF-8")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(missing(prefix)),
        Err(e) => Err(LauncherError::io_at(&e, &reg)),
    };
    let mut text = read()?;
    let section = section_for(exe_file);
    let now = now_secs();
    let mut changed = false;
    let mut replaced = vec![false; existing.len()];
    let mut fresh = Vec::with_capacity(overrides.len());
    for (dll, value) in overrides {
        let edit = apply(&text, &section, dll, value, now);
        changed |= edit.changed;
        let old = existing.iter().position(|o| {
            same_key(&o.dll, dll)
                && same_key(&o.section, &section)
                && same_prefix(&o.prefix, prefix)
        });
        if let Some(i) = old {
            replaced[i] = true;
        }
        // Our own value, from this record or any other of this DLL, is not "what was there".
        let ours = |text: &String| {
            *text == format!("\"{value}\"")
                || existing
                    .iter()
                    .any(|o| same_key(&o.dll, dll) && *text == format!("\"{}\"", o.value))
        };
        fresh.push(match old.map(|i| &existing[i]) {
            Some(o) if edit.line_present => ProtonRecord {
                value: value.to_owned(),
                ..o.clone()
            },
            o => ProtonRecord {
                prefix: prefix.to_path_buf(),
                section: section.clone(),
                dll: dll.to_owned(),
                value: value.to_owned(),
                previous: edit.previous.filter(|p| !ours(p)),
                section_created: edit.section_created || o.is_some_and(|o| o.section_created),
            },
        });
        text = edit.text;
    }
    let records: Vec<ProtonRecord> = existing
        .iter()
        .zip(&replaced)
        .filter(|(_, r)| !**r)
        .map(|(o, _)| o.clone())
        .chain(fresh)
        .collect();
    if records != existing {
        persist(&records)?;
    }
    if changed {
        write_atomic(&reg, text.as_bytes())?;
        let back = read()?;
        for (dll, value) in overrides {
            if current_value(&back, &section, dll).as_deref() != Some(&format!("\"{value}\"")) {
                return Err(LauncherError::io("user.reg did not keep the override")
                    .with("dll", dll.clone())
                    .with("path", reg.display().to_string()));
            }
        }
    }
    Ok(records)
}

/// Restores every record, prefix by prefix (canonical path), each prefix in one atomic write.
/// A prefix or file that is gone is nothing to restore. Refused while a wineserver serves one.
pub(crate) fn restore_prefix(records: &[ProtonRecord]) -> Result<bool> {
    let mut groups: Vec<Vec<&ProtonRecord>> = Vec::new();
    for rec in records {
        match groups
            .iter_mut()
            .find(|g| same_prefix(&g[0].prefix, &rec.prefix))
        {
            Some(g) => g.push(rec),
            None => groups.push(vec![rec]),
        }
    }
    for g in &groups {
        refuse_if_wineserver(&g[0].prefix)?;
    }
    let mut any = false;
    for g in groups {
        let reg = g[0].prefix.join("user.reg");
        check_regular_or_absent(&reg)?;
        let text = match fs::read(&reg) {
            Ok(b) => {
                String::from_utf8(b).map_err(|_| LauncherError::io("user.reg is not UTF-8"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(LauncherError::io_at(&e, &reg)),
        };
        if let Some(new) = restore_all(&text, &g) {
            write_atomic(&reg, new.as_bytes())?;
            any = true;
        }
    }
    Ok(any)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REG: &str = "WINE REGISTRY Version 2\n;; All keys relative to \\\\User\\\\S-1-5-21-0-0-0-1000\n\n#arch=win64\n\n[Control Panel\\\\Desktop] 1700000000\n#time=1da0000000000000\n\"FontSmoothing\"=\"2\"\n\n[Software\\\\Wine\\\\AppDefaults\\\\Other.exe\\\\DllOverrides] 1700000001\n#time=1da0000000000001\n\"dxgi\"=\"native\"\n\n[Software\\\\Wine\\\\DllOverrides] 1700000002\n#time=1da0000000000002\n\"winhttp\"=\"builtin\"\n";

    fn rec(edit: &RegEdit, section: &str) -> ProtonRecord {
        ProtonRecord {
            prefix: "/x".into(),
            section: section.to_owned(),
            dll: "winhttp".into(),
            value: "native,builtin".into(),
            previous: edit.previous.clone(),
            section_created: edit.section_created,
        }
    }

    #[test]
    fn a_new_section_is_added_and_removed_byte_exactly() {
        let key = section_for("Lethal Company.exe");
        let edit = apply(REG, &key, "winhttp", "native,builtin", 1728000000);
        assert!(edit.section_created && edit.changed);
        assert!(edit.text.starts_with(REG.trim_end_matches('\n')));
        assert!(edit.text.contains(
            "\n\n[Software\\\\Wine\\\\AppDefaults\\\\Lethal Company.exe\\\\DllOverrides] 1728000000\n\"winhttp\"=\"native,builtin\"\n"
        ));
        // The global override and the other game's section are untouched.
        assert!(edit.text.contains("[Software\\\\Wine\\\\DllOverrides] 1700000002\n#time=1da0000000000002\n\"winhttp\"=\"builtin\"\n"));
        let back = restore(&edit.text, &rec(&edit, &key)).unwrap();
        assert_eq!(back, REG);
    }

    #[test]
    fn an_existing_value_is_replaced_and_put_back() {
        let key = section_for("Other.exe");
        let with = apply(REG, &key, "dxgi", "native", 1);
        assert!(!with.changed);
        let edit = apply(REG, &key, "winhttp", "native,builtin", 1);
        assert!(!edit.section_created && edit.previous.is_none());
        assert!(edit.text.contains("\"dxgi\"=\"native\"\n\"winhttp\"=\"native,builtin\"\n\n[Software\\\\Wine\\\\DllOverrides]"));
        assert_eq!(restore(&edit.text, &rec(&edit, &key)).unwrap(), REG);

        let pre = REG.replace("\"dxgi\"=\"native\"", "\"WinHttp\"=\"builtin\"");
        let edit = apply(&pre, &key, "winhttp", "native,builtin", 1);
        assert_eq!(edit.previous.as_deref(), Some("\"builtin\""));
        assert!(edit.text.contains("\"winhttp\"=\"native,builtin\"\n"));
        let back = restore(&edit.text, &rec(&edit, &key)).unwrap();
        assert_eq!(
            back,
            pre.replace("\"WinHttp\"=\"builtin\"", "\"winhttp\"=\"builtin\"")
        );
    }

    #[test]
    fn a_value_someone_changed_since_is_left() {
        let key = section_for("Game.exe");
        let edit = apply(REG, &key, "winhttp", "native,builtin", 1);
        let changed = edit
            .text
            .replace("\"winhttp\"=\"native,builtin\"", "\"winhttp\"=\"native\"");
        let back = restore(&changed, &rec(&edit, &key));
        assert!(
            back.is_none(),
            "the section still holds a value that is not ours"
        );
    }

    #[test]
    fn crlf_and_escaped_names_survive() {
        let text = REG.replace('\n', "\r\n");
        let key = section_for("Game [GOTY] é.exe");
        let edit = apply(&text, &key, "winhttp", "native,builtin", 5);
        assert!(edit.text.contains(
            "[Software\\\\Wine\\\\AppDefaults\\\\Game \\[GOTY\\] \\x00e9.exe\\\\DllOverrides] 5\r\n"
        ));
        assert_eq!(
            header_key(
                "[Software\\\\Wine\\\\AppDefaults\\\\Game \\[GOTY\\] \\x00e9.exe\\\\DllOverrides] 5"
            )
            .unwrap(),
            key
        );
        assert_eq!(restore(&edit.text, &rec(&edit, &key)).unwrap(), text);
    }

    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn tmp(name: &str) -> Dir {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "agl-proton-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Dir(fs::canonicalize(&p).unwrap())
    }
    fn prefix_with(text: &str) -> Dir {
        let d = tmp("pfx");
        fs::write(d.0.join("user.reg"), text).unwrap();
        d
    }
    fn ov(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(d, v)| (d.to_string(), v.to_string()))
            .collect()
    }
    fn reg_of(d: &Dir) -> String {
        fs::read_to_string(d.0.join("user.reg")).unwrap()
    }
    fn keep(_: &[ProtonRecord]) -> Result<()> {
        Ok(())
    }

    #[test]
    fn two_spellings_of_one_prefix_are_one_record() {
        // `~/.local/share/Steam/...` and `~/.steam/steam/...`, the second through Steam's link.
        let steam = tmp("local-share-steam");
        let real = steam.0.join("steamapps/compatdata/1/pfx");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("user.reg"), REG).unwrap();
        let home = tmp("dot-steam");
        std::os::unix::fs::symlink(&steam.0, home.0.join("steam")).unwrap();
        let linked = home.0.join("steam/steamapps/compatdata/1/pfx");
        let o = ov(&[("dxgi", "native,builtin"), ("d3d11", "native")]);
        let first = apply_prefix(&real, "Game.exe", &o, &[], keep).unwrap();
        let again = apply_prefix(&linked, "Game.exe", &o, &first, keep).unwrap();
        assert_eq!(
            again.len(),
            2,
            "one record per DLL, not one per spelling: {again:?}"
        );
        assert_eq!(again, first, "the first records, kept");
        assert!(restore_prefix(&again).unwrap());
        assert_eq!(fs::read_to_string(real.join("user.reg")).unwrap(), REG);
    }

    #[test]
    fn our_own_value_is_never_recorded_as_the_users() {
        // Our value is already there and nothing records it (a lost ledger, another spelling).
        let key = section_for("Game.exe");
        let with_ours = apply(REG, &key, "dxgi", "native,builtin", 1).text;
        let p = prefix_with(&with_ours);
        let recs = apply_prefix(
            &p.0,
            "Game.exe",
            &ov(&[("dxgi", "native,builtin")]),
            &[],
            keep,
        )
        .unwrap();
        assert_eq!(recs[0].previous, None, "{recs:?}");
        // A value an older record wrote for that DLL is ours too.
        let older = ProtonRecord {
            prefix: "/elsewhere/pfx".into(),
            section: key.clone(),
            dll: "dxgi".into(),
            value: "native,builtin".into(),
            previous: None,
            section_created: true,
        };
        let recs =
            apply_prefix(&p.0, "Game.exe", &ov(&[("dxgi", "native")]), &[older], keep).unwrap();
        let new = recs.iter().find(|r| r.prefix == p.0).unwrap();
        assert_eq!(new.previous, None, "{recs:?}");
    }

    #[test]
    fn a_record_of_another_section_is_kept_and_both_are_restored() {
        let p = prefix_with(REG);
        let o = ov(&[("dxgi", "native,builtin")]);
        let old = apply_prefix(&p.0, "Old.exe", &o, &[], keep).unwrap();
        let both = apply_prefix(&p.0, "New.exe", &o, &old, keep).unwrap();
        assert_eq!(both.len(), 2, "{both:?}");
        assert_eq!(both[0], old[0], "the old record first, unchanged");
        restore_prefix(&both).unwrap();
        assert_eq!(reg_of(&p), REG);
    }

    #[test]
    fn the_ledger_is_written_before_user_reg_and_a_failure_there_touches_nothing() {
        let p = prefix_with(REG);
        let e = apply_prefix(&p.0, "Game.exe", &ov(&[("dxgi", "native")]), &[], |_| {
            Err(LauncherError::io("disk full"))
        })
        .unwrap_err();
        assert_eq!(e.code, codes::IO);
        assert_eq!(
            reg_of(&p),
            REG,
            "user.reg untouched when the ledger could not be written"
        );
    }

    #[test]
    fn a_section_is_dropped_whichever_record_is_restored_first() {
        let key = section_for("Game.exe");
        let a = apply(REG, &key, "dxgi", "native", 1);
        let b = apply(&a.text, &key, "d3d11", "native", 1);
        let ra = ProtonRecord {
            dll: "dxgi".into(),
            value: "native".into(),
            ..rec(&a, &key)
        };
        let rb = ProtonRecord {
            dll: "d3d11".into(),
            value: "native".into(),
            ..rec(&b, &key)
        };
        assert!(ra.section_created && !rb.section_created);
        // The creator's line goes FIRST here (restore_all walks the list backwards).
        assert_eq!(restore_all(&b.text, &[&rb, &ra]).unwrap(), REG);
        assert_eq!(restore_all(&b.text, &[&ra, &rb]).unwrap(), REG);
    }

    #[test]
    fn a_live_wineserver_blocks_apply_and_restore_and_a_stale_one_does_not() {
        use std::os::unix::fs::MetadataExt;
        let p = prefix_with(REG);
        let root = tmp("wine-root");
        TEST_SERVER_ROOT.with(|r| *r.borrow_mut() = Some(root.0.clone()));
        let meta = fs::metadata(&p.0).unwrap();
        let dir = root
            .0
            .join(format!("server-{:x}-{:x}", meta.dev(), meta.ino()));
        fs::create_dir_all(&dir).unwrap();
        let o = ov(&[("dxgi", "native")]);

        let listener = std::os::unix::net::UnixListener::bind(dir.join("socket")).unwrap();
        let e = apply_prefix(&p.0, "Game.exe", &o, &[], keep).unwrap_err();
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::GAME_RUNNING, Some("wineserver"))
        );
        assert_eq!(reg_of(&p), REG);

        // Stale: the server is gone, its directory and socket file are not.
        drop(listener);
        let recs = apply_prefix(&p.0, "Game.exe", &o, &[], keep).unwrap();
        // Alive again: restore waits too.
        fs::remove_file(dir.join("socket")).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(dir.join("socket")).unwrap();
        let e = restore_prefix(&recs).unwrap_err();
        assert_eq!(e.code, codes::GAME_RUNNING);
        TEST_SERVER_ROOT.with(|r| *r.borrow_mut() = None);
    }

    #[test]
    fn a_wine_rewrite_that_moved_our_section_still_restores() {
        let key = section_for("Aaa.exe");
        let edit = apply(REG, &key, "winhttp", "native,builtin", 1);
        // Wine sorts keys when it saves; put our section in the middle.
        let ours = "[Software\\\\Wine\\\\AppDefaults\\\\Aaa.exe\\\\DllOverrides] 1\n#time=1\n\"winhttp\"=\"native,builtin\"\n\n";
        let moved = REG.replace(
            "[Software\\\\Wine\\\\AppDefaults\\\\Other.exe",
            &format!("{ours}[Software\\\\Wine\\\\AppDefaults\\\\Other.exe"),
        );
        assert_eq!(restore(&moved, &rec(&edit, &key)).unwrap(), REG);
    }
}
