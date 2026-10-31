// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Canary K6: engine knowledge leaves the detector's code for data (engine descriptors), and
//! never comes back. This counts the string literals in `src/engines/**/*.rs`, `src/detect.rs`
//! and the plan's gates (`src/plan.rs`, `src/manifest.rs`) that name an engine or one of its
//! tell-tale files, and holds the count to [`CEILING`] EXACTLY: a rise fails, and so does a fall
//! until the same commit lowers the constant — so the count can only ever fall, on paper and in
//! fact. Data files (`src/engines/builtin/*.toml`) are where those names belong and are not
//! counted; neither are comments, nor the bodies of `#[cfg(test)] mod`s (a test names the
//! engines it tests). `doorstop.rs`, `mods/` and `former.rs` are loader and history code, not
//! detection, and are not scanned (plan B §2).

use std::fs;
use std::path::{Path, PathBuf};

/// The count at launcher 0.5.0's L2 (43 at L1): the built-in rules left `src/detect.rs` for
/// `src/engines/builtin/*.toml`. What is left: [`Engine`]'s own codes (8, `code()` and
/// `from_code()`), the wrapper name `crashreportclient` (1), the three built-ins' file names in
/// `include_str!` (3), and descriptor.rs's reserved words (5). May only go down, and equals the
/// count.
///
/// [`Engine`]: astra_game_launcher::Engine
const CEILING: usize = 17;

/// Matched case-insensitively inside each literal.
const ENGINE_WORDS: &[&str] = &[
    "unity",
    "unreal",
    "godot",
    "il2cpp",
    "mono",
    "gdpc",
    ".pck",
    "_data",
    "paks",
    "content",
    "binaries",
    "win64",
    "shipping",
    "gameassembly",
    "assembly-csharp",
    "managed",
    "globalgamemanagers",
    "maindata",
    "crashreportclient",
];

/// The string literals of Rust source `src` (plain, raw, byte), comments and the bodies of
/// `#[cfg(test)] mod`s skipped.
fn literals(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // Braces outside literals and comments; inside a test module, the depth its body opened at.
    let mut depth = 0usize;
    let mut test_mod: Option<usize> = None;
    let mut test_mod_next = false;
    let push = |out: &mut Vec<String>, lit: &str, skipping: bool| {
        if !skipping {
            out.push(lit.to_owned());
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
                i += 1;
            }
            b'}' => {
                if test_mod == Some(depth) {
                    test_mod = None;
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
                } else {
                    let ch_len = src[i + 1..].chars().next().map_or(1, char::len_utf8);
                    if b.get(i + 1 + ch_len) == Some(&b'\'') {
                        i += 2 + ch_len;
                    } else {
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
                push(&mut out, &src[body..end], skipping);
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
                push(&mut out, &src[start..i.min(b.len())], skipping);
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
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

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn naming(literal: &str) -> bool {
    let lower = literal.to_ascii_lowercase();
    ENGINE_WORDS.iter().any(|w| lower.contains(w))
}

#[test]
fn engine_names_in_the_detectors_code_only_ever_fall() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![
        root.join("src/detect.rs"),
        root.join("src/plan.rs"),
        root.join("src/manifest.rs"),
    ];
    rust_files(&root.join("src/engines"), &mut files);
    files.sort();
    assert!(files.len() >= 6, "{files:?}");
    let mut total = 0;
    let mut report = String::new();
    for f in &files {
        let src = fs::read_to_string(f).unwrap();
        let n = literals(&src).iter().filter(|l| naming(l)).count();
        report.push_str(&format!(
            "  {n:4}  {}\n",
            f.strip_prefix(root).unwrap().display()
        ));
        total += n;
    }
    eprintln!("engine-name literals: {total} (ceiling {CEILING})\n{report}");
    assert!(
        total <= CEILING,
        "engine names crept back into the detector's code: {total} > {CEILING}\n{report}"
    );
    assert_eq!(
        total, CEILING,
        "the count fell: lower CEILING to {total} in this commit, so it can never rise back\n{report}"
    );
}

#[test]
fn the_scrape_sees_literals_and_nothing_else() {
    let src = r####"
        // "unity" in a comment
        /* "unreal" /* nested "godot" */ still a comment */
        let a = "UnityPlayer.dll";
        let b = r#"Content/"Paks""#;
        let c = b"GDPC";
        let d = br"x.pck";
        let e: &'static str = "plain";
        let f = '"';
        let g = "esc \" Managed";
        let h = 'x';
        let i = r##"a "# b"##;
        fn f() { let j = "{name}_Data"; let k = '{'; }
        #[cfg(test)]
        mod tests {
            fn t() { let x = "UnityPlayer.so"; if y { let z = r"}"; } }
        }
        #[cfg(test)]
        fn not_a_module() -> &'static str { "il2cpp" }
        let after = "Managed";
    "####;
    let l = literals(src);
    assert_eq!(
        l,
        [
            "UnityPlayer.dll",
            "Content/\"Paks\"",
            "GDPC",
            "x.pck",
            "plain",
            "esc \\\" Managed",
            "a \"# b",
            "{name}_Data",
            "il2cpp",
            "Managed",
        ]
    );
    assert_eq!(l.iter().filter(|x| naming(x)).count(), 8);
}
