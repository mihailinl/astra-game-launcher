// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Reading this crate's own sources for the canaries that hold them (K6 `tests/no_engine_names.rs`,
//! K16 `tests/anti_cheat.rs`): the string literals of a Rust file, and the Rust files below a
//! folder. `tests/no_engine_names.rs` `the_scrape_sees_literals_and_nothing_else` pins the scrape.

use std::fs;
use std::path::{Path, PathBuf};

/// The string literals of Rust source `src` (plain, raw, byte), comments and the bodies of
/// `#[cfg(test)] mod`s skipped.
pub fn literals(src: &str) -> Vec<String> {
    scan(src).0
}

/// Rust source `src` as the compiler reads its expressions: comments gone, every string and char
/// literal `""`, the bodies of `#[cfg(test)] mod`s gone. What a canary bound to expressions (a
/// call, a method) reads, so that a name in a comment, a doc or a message never matches.
pub fn code(src: &str) -> String {
    scan(src).1
}

/// [`literals`] and [`code`], in one pass.
fn scan(src: &str) -> (Vec<String>, String) {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut code: Vec<u8> = Vec::new();
    let mut i = 0;
    // Braces outside literals and comments; inside a test module, the depth its body opened at.
    let mut depth = 0usize;
    let mut test_mod: Option<usize> = None;
    let mut test_mod_next = false;
    let push = |out: &mut Vec<String>, code: &mut Vec<u8>, lit: &str, skipping: bool| {
        if !skipping {
            out.push(lit.to_owned());
            code.extend_from_slice(b"\"\"");
        }
    };
    while i < b.len() {
        let skipping = test_mod.is_some();
        match b[i] {
            b'#' if src[i..].starts_with("#[cfg(test)]") => {
                i += "#[cfg(test)]".len();
                let rest = src[i..].trim_start();
                if rest.starts_with("mod ") || rest.starts_with("pub mod ") {
                    test_mod_next = true;
                }
            }
            b'{' => {
                depth += 1;
                if test_mod_next && test_mod.is_none() {
                    test_mod = Some(depth);
                }
                test_mod_next = false;
                if test_mod.is_none() {
                    code.push(b'{');
                }
                i += 1;
            }
            b'}' => {
                if test_mod == Some(depth) {
                    test_mod = None;
                } else if test_mod.is_none() {
                    code.push(b'}');
                }
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b';' if test_mod_next => {
                // `#[cfg(test)] mod x;`: a file of its own, not scanned here.
                test_mod_next = false;
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0;
                while i < b.len() {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'\'' => {
                // A char literal ('x', '\n', '\u{..}') or a lifetime ('a).
                if b.get(i + 1) == Some(&b'\\') {
                    i += 2;
                    while i < b.len() && b[i] != b'\'' {
                        i += 1;
                    }
                    i += 1;
                    if !skipping {
                        code.extend_from_slice(b"''");
                    }
                } else {
                    let ch_len = src[i + 1..].chars().next().map_or(1, char::len_utf8);
                    if b.get(i + 1 + ch_len) == Some(&b'\'') {
                        i += 2 + ch_len;
                        if !skipping {
                            code.extend_from_slice(b"''");
                        }
                    } else {
                        if !skipping {
                            code.push(b'\'');
                        }
                        i += 1;
                    }
                }
            }
            b'r' | b'b'
                if !(i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
                    && raw_start(b, i).is_some() =>
            {
                let (hashes, body) = raw_start(b, i).unwrap();
                let close: Vec<u8> = std::iter::once(b'"')
                    .chain(std::iter::repeat_n(b'#', hashes))
                    .collect();
                let end = b[body..]
                    .windows(close.len())
                    .position(|w| w == close.as_slice())
                    .map_or(b.len(), |p| body + p);
                push(&mut out, &mut code, &src[body..end], skipping);
                i = end + close.len();
            }
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                push(&mut out, &mut code, &src[start..i.min(b.len())], skipping);
                i += 1;
            }
            c => {
                if !skipping {
                    code.push(c);
                }
                i += 1;
            }
        }
    }
    (out, String::from_utf8_lossy(&code).into_owned())
}

/// `r"`, `r#"`, `br"`, `b"`…: the number of `#`s and where the body starts.
fn raw_start(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut j = i;
    if b[j] == b'b' {
        j += 1;
        if b.get(j) == Some(&b'"') {
            return Some((0, j + 1));
        }
    }
    if b.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let mut hashes = 0;
    while b.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
    }
    (b.get(j) == Some(&b'"')).then_some((hashes, j + 1))
}

/// Every `.rs` file below `dir`, appended to `out`.
pub fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}
