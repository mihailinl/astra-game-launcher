// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SHA-256 helpers, RFC 8785 (JCS): the canonical JSON a digest of a JSON value is of, and the
//! one reading of a TOML text's newlines a digest of what is parsed out of it needs.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Streams a file through SHA-256. The caller has already refused symbolic links.
pub(crate) fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// A SHA-256 written by a person or a manifest: 64 hex digits, any case. Returns it lowercased.
pub(crate) fn normalize_hex(s: &str) -> Option<String> {
    let s = s.trim();
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// A TOML text as a digest of what is parsed out of it reads it: every raw CRLF made LF before
/// the parse. [`crate::Manifest::parse`] (so [`crate::link_digest`] and `[integration.engine]`'s
/// digest) and [`crate::Descriptor::from_toml`] parse this, and any other reader whose values
/// reach a digest must too.
///
/// TOML 1.0 leaves a newline inside a multi-line string to the parser, and git's `autocrlf`
/// checks a release's `astra-gi.toml` out with CRLF on Windows. The `toml` crate keeps the CRLF
/// in the string; the specification's reference (Python's `tomllib`, AstraPlugins
/// `tools/check-games-goldens.py`) reads it as LF, because it replaces every CRLF of the text
/// before it parses — this is that replacement. Without it the same shipped bytes were two
/// consent digests, the catalogue's and the launcher's (`manifest.md` §2.6.1). A raw CR is legal
/// in TOML only before an LF, so only raw newlines change: `\r\n` written as escapes keeps its
/// CR, and a lone CR is still the parse's refusal.
pub fn toml_newlines(text: &str) -> Cow<'_, str> {
    if text.contains("\r\n") {
        Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// RFC 8785 (JCS), the one canonical JSON in this crate: an engine descriptor's (its digest,
/// [`crate::Descriptor::digest`]) and `[integration.link]`'s ([`crate::link_digest`]). Object
/// keys sorted by their UTF-16 code units, no whitespace, integers in decimal, strings with only
/// `"`, `\` and controls escaped. For the values its callers admit: a descriptor's integers are
/// at most 2^53 - 1 (`check_value`), and a link holds no number at all — a float, which would need
/// ECMAScript's number formatting, never reaches it.
pub(crate) fn jcs(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => jcs_string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                jcs(v, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                jcs_string(k, out);
                out.push(':');
                jcs(&o[k], out);
            }
            out.push('}');
        }
    }
}

fn jcs_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8785 sorts keys by UTF-16 code units: U+10000 (a surrogate pair, D800 DC00) before
    /// U+E000, the other way round from UTF-8 and from a `BTreeMap<String, _>`.
    #[test]
    fn jcs_sorts_object_keys_by_utf16() {
        let v: Value = serde_json::from_str(
            r#"{"b":1,"\ue000":2,"\ud800\udc00":3,"a":[true,"x\u0001\"\\"],"B":{"z":0,"y":-5}}"#,
        )
        .unwrap();
        let mut out = String::new();
        jcs(&v, &mut out);
        assert_eq!(
            out,
            "{\"B\":{\"y\":-5,\"z\":0},\"a\":[true,\"x\\u0001\\\"\\\\\"],\"b\":1,\"\u{10000}\":3,\"\u{e000}\":2}"
        );
    }

    /// RFC 8785 §3.2.2.2: a control with no short escape is `\u` and four LOWER-case hex digits;
    /// DEL and everything past it are written as themselves.
    #[test]
    fn jcs_writes_controls_in_lower_case_hex() {
        let v = Value::String("\u{000b}\u{001f}\u{001e}\u{007f}\u{0080}".into());
        let mut out = String::new();
        jcs(&v, &mut out);
        assert_eq!(out, "\"\\u000b\\u001f\\u001e\u{7f}\u{80}\"");
    }
}
