// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `[[config_writes]]`: sets keys in one section of an INI file (BepInEx's `.cfg` is one), and
//! leaves every other line as it was.

fn bare(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

fn section_name(line: &str) -> Option<&str> {
    let t = bare(line).trim();
    t.strip_prefix('[')?.strip_suffix(']').map(str::trim)
}

fn key_of(line: &str) -> Option<&str> {
    let t = bare(line).trim_start();
    if t.starts_with('#') || t.starts_with(';') || t.starts_with('[') {
        return None;
    }
    t.split_once('=').map(|(k, _)| k.trim())
}

/// Sets `keys` in `[section]` of `text`. Missing keys go at the end of the section, a missing
/// section at the end of the file. Line endings follow the file.
pub(crate) fn set_keys(text: &str, section: &str, keys: &[(String, String)]) -> String {
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(b) => ("\u{feff}", b),
        None => ("", text),
    };
    let cr = if body.contains("\r\n") { "\r" } else { "" };
    let mut lines: Vec<String> = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').map(str::to_owned).collect()
    };
    // A file that ends with a newline splits into a last empty line; keep it last.
    let trailing_empty = lines.last().is_some_and(|l| l.is_empty());
    if trailing_empty {
        lines.pop();
    }

    let find = |lines: &[String]| -> Option<(usize, usize)> {
        let start = lines
            .iter()
            .position(|l| section_name(l).is_some_and(|n| n.eq_ignore_ascii_case(section)))?;
        let end = lines[start + 1..]
            .iter()
            .position(|l| section_name(l).is_some())
            .map_or(lines.len(), |p| start + 1 + p);
        Some((start, end))
    };

    if find(&lines).is_none() {
        if lines.last().is_some_and(|l| !bare(l).trim().is_empty()) {
            lines.push(cr.to_owned());
        }
        lines.push(format!("[{section}]{cr}"));
    }
    for (k, v) in keys {
        let (start, end) = find(&lines).expect("the section exists now");
        if let Some(i) = (start + 1..end)
            .find(|&i| key_of(&lines[i]).is_some_and(|key| key.eq_ignore_ascii_case(k)))
        {
            let line = bare(&lines[i]).to_owned();
            let eq = line.find('=').unwrap_or(line.len());
            let spaced = line[eq + 1..].starts_with(' ');
            let lhs = &line[..eq + 1];
            lines[i] = if spaced {
                format!("{lhs} {v}{cr}")
            } else {
                format!("{lhs}{v}{cr}")
            };
        } else {
            let at = (start..end)
                .rev()
                .find(|&i| !bare(&lines[i]).trim().is_empty())
                .unwrap_or(start)
                + 1;
            lines.insert(at, format!("{k} = {v}{cr}"));
        }
    }
    let mut out = String::from(bom);
    out.push_str(&lines.join("\n"));
    out.push('\n');
    if !cr.is_empty() {
        // `join` put `\n` after each `…\r`; the last line needs its `\r` before the final `\n`.
        if !out.ends_with("\r\n") {
            out.insert(out.len() - 1, '\r');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn sets_existing_and_new_keys_in_place() {
        let cfg = "## Settings file\n\n[Connection]\n\n## The port\n# Setting type: Int32\nPort = 0\n\n[Other]\nPort = 7\n";
        let out = set_keys(
            cfg,
            "Connection",
            &kv(&[("Port", "5555"), ("Token", "abc")]),
        );
        assert_eq!(
            out,
            "## Settings file\n\n[Connection]\n\n## The port\n# Setting type: Int32\nPort = 5555\nToken = abc\n\n[Other]\nPort = 7\n"
        );
    }

    #[test]
    fn adds_a_missing_section_and_creates_from_nothing() {
        let out = set_keys("[A]\nx=1\n", "Connection", &kv(&[("Port", "1")]));
        assert_eq!(out, "[A]\nx=1\n\n[Connection]\nPort = 1\n");
        assert_eq!(
            set_keys("", "Connection", &kv(&[("Port", "1")])),
            "[Connection]\nPort = 1\n"
        );
    }

    #[test]
    fn keeps_crlf_and_unspaced_style() {
        let out = set_keys(
            "[Connection]\r\nPort=0\r\n",
            "Connection",
            &kv(&[("Port", "9")]),
        );
        assert_eq!(out, "[Connection]\r\nPort=9\r\n");
    }
}
