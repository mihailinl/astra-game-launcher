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

/// Puts back what [`apply`] replaced, when the value is still ours. Returns the new text when
/// anything changed.
pub(crate) fn restore(text: &str, rec: &ProtonRecord) -> Option<String> {
    let (mut lines, cr) = split(text);
    let (start, end) = find_section(&lines, &rec.section)?;
    let ours_text = format!("\"{}\"", rec.value);
    let mut changed = false;
    if let Some(i) = find_value(&lines, start, end, &rec.dll) {
        let (name_line, current) = value_line(&lines[i]).unwrap_or_default();
        let _ = name_line;
        if current == ours_text {
            match &rec.previous {
                Some(prev) => {
                    let new = format!("\"{}\"={prev}{cr}", rec.dll);
                    if new != lines[i] {
                        lines[i] = new;
                        changed = true;
                    }
                }
                None => {
                    lines.remove(i);
                    changed = true;
                }
            }
        }
    }
    if rec.section_created {
        let (start, end) = find_section(&lines, &rec.section)?;
        let has_values = (start + 1..end).any(|i| {
            let l = bare(&lines[i]);
            l.starts_with('"') || l.starts_with('@')
        });
        if !has_values {
            let stop = last_non_blank(&lines, start, end) + 1;
            let from = if start > 0 && bare(&lines[start - 1]).trim().is_empty() {
                start - 1
            } else {
                start
            };
            lines.drain(from..stop);
            changed = true;
        }
    }
    changed.then(|| lines.join("\n"))
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

/// Applies the override in `prefix/user.reg`. Keeps the first record's "before" across launches.
pub(crate) fn apply_prefix(
    prefix: &Path,
    exe_file: &str,
    dll: &str,
    value: &str,
    existing: Option<&ProtonRecord>,
) -> Result<ProtonRecord> {
    let reg = prefix.join("user.reg");
    if !fs::symlink_metadata(prefix).is_ok_and(|m| m.is_dir()) {
        return Err(missing(prefix));
    }
    check_regular_or_absent(&reg)?;
    let text = match fs::read(&reg) {
        Ok(b) => String::from_utf8(b).map_err(|_| LauncherError::io("user.reg is not UTF-8"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(missing(prefix)),
        Err(e) => return Err(LauncherError::io_at(&e, &reg)),
    };
    let section = section_for(exe_file);
    let edit = apply(&text, &section, dll, value, now_secs());
    if edit.changed {
        write_atomic(&reg, edit.text.as_bytes())?;
    }
    let fresh = ProtonRecord {
        prefix: prefix.to_path_buf(),
        section: section.clone(),
        dll: dll.to_owned(),
        value: value.to_owned(),
        previous: edit.previous,
        section_created: edit.section_created,
    };
    Ok(match existing {
        Some(old)
            if edit.line_present
                && old.prefix == fresh.prefix
                && same_key(&old.section, &section)
                && same_key(&old.dll, dll) =>
        {
            ProtonRecord {
                value: value.to_owned(),
                ..old.clone()
            }
        }
        _ => fresh,
    })
}

/// Restores what [`apply_prefix`] recorded. A prefix or file that is gone is nothing to restore.
pub(crate) fn restore_prefix(rec: &ProtonRecord) -> Result<bool> {
    let reg = rec.prefix.join("user.reg");
    check_regular_or_absent(&reg)?;
    let text = match fs::read(&reg) {
        Ok(b) => String::from_utf8(b).map_err(|_| LauncherError::io("user.reg is not UTF-8"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(LauncherError::io_at(&e, &reg)),
    };
    match restore(&text, rec) {
        Some(new) => {
            write_atomic(&reg, new.as_bytes())?;
            Ok(true)
        }
        None => Ok(false),
    }
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
