// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TOML 1.0: what the specification parses a release's `astra-gi.toml` and an engine descriptor
//! as (AstraPlugins `spec/games/manifest.md` §2.6.1 step 1, "parsed as TOML 1.0"; its reference,
//! `tools/check-games-goldens.py`, reads with Python's `tomllib`).
//!
//! The launcher parses with the `toml` crate, which reads TOML 1.1: the `\e` and `\xHH` escapes,
//! newlines and a trailing comma inside an inline table, a time without seconds, and a leading
//! byte-order mark besides. A release written so installs here and is refused at ingest, so a
//! digest `check` reported for it (`link.digest`, the descriptor's) is one no catalogue entry
//! ever carries. `check` and `validate-descriptor` hold the text to TOML 1.0 with [`refusal`].

/// Why `text`, which the launcher's own parser reads, is not TOML 1.0; `None` when it is, and
/// when the launcher's parser refuses it as well (that refusal is the finding already).
///
/// The 1.0 parser is `toml_edit` 0.22 (Cargo.toml says why that release). It skips a leading
/// byte-order mark, which TOML 1.0's grammar has no place for and `tomllib` refuses, so that is
/// asked first. It runs with no nesting limit of its own, and only on a text `toml` has read
/// (which nests 80 deep at most), so nothing the launcher accepts is refused for its depth here.
pub fn refusal(text: &str) -> Option<String> {
    toml::from_str::<toml::Table>(text).ok()?;
    if text.starts_with('\u{FEFF}') {
        return Some("a byte-order mark before the first line: TOML 1.0 has none".to_owned());
    }
    let e = text.parse::<toml_edit::DocumentMut>().err()?;
    // Counted in bytes: a span may start inside a character.
    let at = e.span().map(|s| {
        let before = &text.as_bytes()[..s.start.min(text.len())];
        before.iter().filter(|b| **b == b'\n').count() + 1
    });
    let what = e.message().lines().collect::<Vec<_>>().join("; ");
    Some(match at {
        Some(line) => format!("not TOML 1.0 (line {line}): {what}"),
        None => format!("not TOML 1.0: {what}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each of TOML 1.1's additions the launcher's parser reads is refused, with its line; TOML
    /// 1.0 is not, nor CRLF, nor 80 nested arrays (the launcher's own limit); a text the
    /// launcher's parser refuses is its refusal, not this one's. Every refused input here is
    /// refused by `tomllib` (Python 3.14) too, and every accepted one accepted.
    #[test]
    fn toml_1_1_is_refused_and_toml_1_0_is_not() {
        for (name, text) in [
            ("a BOM", "\u{FEFF}a = 1\n"),
            ("\\e", "a = 1\nb = \"\\e\"\n"),
            ("\\x", "a = 1\nb = \"drop\\x5Fitem\"\n"),
            ("a multi-line inline table", "a = {\n b = 1 }\n"),
            ("a trailing comma", "a = { b = 1, }\n"),
            ("a time without seconds", "a = 07:32\n"),
            ("a date-time without seconds", "a = 1979-05-27T07:32Z\n"),
        ] {
            assert!(
                toml::from_str::<toml::Table>(text).is_ok(),
                "{name}: the launcher reads it"
            );
            let why = refusal(text).unwrap_or_else(|| panic!("{name}: not refused"));
            assert!(why.contains("TOML 1.0"), "{name}: {why}");
        }
        assert_eq!(
            refusal("a = 1\nb = \"\\e\"\n").unwrap().split(':').next(),
            Some("not TOML 1.0 (line 2)")
        );
        let deep = |d: usize| format!("a = {}1{}\n", "[".repeat(d), "]".repeat(d));
        for (name, text) in [
            (
                "TOML 1.0",
                "a = \"\\u00e9\\n\"\nb = { c = [1, 2] }\nt = 07:32:00\n".to_owned(),
            ),
            ("CRLF", "a = \"\"\"\r\nx\r\ny\"\"\"\r\nb = 2\r\n".to_owned()),
            ("80 deep", deep(80)),
            ("the launcher's refusal", "a = \n".to_owned()),
            ("81 deep, the launcher's refusal", deep(81)),
        ] {
            assert_eq!(refusal(&text), None, "{name}");
        }
    }
}
