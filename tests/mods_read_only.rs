// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The player's mods are read-only and private (engine-descriptor-v1.md §7.10), held in the
//! source: the code that finds and judges them — `src/mods.rs`, `src/mods/**` and `src/engines/**`
//! (the descriptors, the mods evaluation, its probes, `builtin_placement`) — calls no write and no
//! log, but for the writes [`ALLOWED`] names by the function they are in: each one of a file in
//! Astra's own profile (`mods-choice.json` and its lock; what a Play with mods left there).
//!
//! The scan reads expressions, never prose: comments, string and char literals and the bodies of
//! `#[cfg(test)] mod`s are gone before it looks (`common::scrape::code`), so a write named in a
//! doc, a message or a test never counts, and one written as code always does.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use common::scrape::{code, rust_files};

/// Every call or flag that creates, changes, renames, moves or deletes a file or a folder, or
/// opens one for writing: std's, the open flags of both platforms, and this crate's own writers.
const WRITES: &[&str] = &[
    "fs::write(",
    "File::create(",
    "File::create_new(",
    "File::options(",
    ".write(",
    ".append(",
    ".create(",
    ".create_new(",
    ".truncate(",
    ".set_len(",
    "write_all(",
    "remove_file(",
    "remove_dir(",
    "remove_dir_all(",
    "rename(",
    "fs::copy(",
    "create_dir(",
    "create_dir_all(",
    "set_permissions(",
    "set_modified(",
    "set_times(",
    "hard_link(",
    "symlink(",
    "symlink_file(",
    "symlink_dir(",
    "write_atomic(",
    "write_atomic_private(",
    "prune_empty_dirs(",
    "ledger::write(",
    "ledger::remove(",
    "ProfileLock::acquire(",
    "O_WRONLY",
    "O_RDWR",
    "O_CREAT",
    "O_TRUNC",
    "O_APPEND",
    "GENERIC_WRITE",
    "FILE_GENERIC_WRITE",
    "FILE_APPEND_DATA",
];

/// Every way a line reaches a log, a terminal or a caller's progress: none, anywhere here (§7.10:
/// not even "with mods" or "without" in a log a problem report attaches).
const LOGS: &[&str] = &[
    "println!",
    "eprintln!",
    "print!",
    "eprint!",
    "dbg!",
    "tracing::",
    "log::",
    ".report(",
    ".progress",
];

/// The writes the mods code makes, each into Astra's own profile: `(file, function, call)`.
const ALLOWED: &[(&str, &str, &str)] = &[
    // `mods-choice.json`, atomically, under the profile's lock.
    ("src/mods/choice.rs", "write", "write_atomic("),
    (
        "src/mods/choice.rs",
        "set_mods_choice",
        "ProfileLock::acquire(",
    ),
    (
        "src/mods/choice.rs",
        "add_mods_root",
        "ProfileLock::acquire(",
    ),
    // Uninstall takes the choice away with the integration.
    ("src/mods/choice.rs", "remove", "remove_file("),
    // Uninstall takes away `chain.ini` and the chain's `last-run.txt`, then their folder.
    ("src/mods/play.rs", "remove_play_files", "remove_file("),
    ("src/mods/play.rs", "remove_play_files", "prune_empty_dirs("),
];

/// Whether `token` at `at` in `src` stands alone: not the tail of a longer name.
fn alone(src: &str, at: usize, token: &str) -> bool {
    if !token.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
        return true;
    }
    !src[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Each `fn` of `src` (a code view): its name and the span of its body.
fn functions(src: &str) -> Vec<(String, usize, usize)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = src[from..].find("fn ") {
        let at = from + found;
        from = at + 3;
        if !alone(src, at, "fn ") {
            continue;
        }
        let name: String = src[at + 3..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        // The body: the first `{` before any `;` (a trait method with no body has none).
        let rest = &src[at..];
        let (Some(open), semi) = (rest.find('{'), rest.find(';')) else {
            continue;
        };
        if semi.is_some_and(|s| s < open) {
            continue;
        }
        let open = at + open;
        let mut depth = 0usize;
        let mut close = b.len();
        for (i, c) in b.iter().enumerate().skip(open) {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push((name, open, close));
    }
    out
}

/// Every `(function, token)` of `tokens` the code view of `src` holds, each found in the
/// innermost function around it (`""` outside any).
fn calls(src: &str, tokens: &[&str]) -> BTreeSet<(String, String)> {
    let view = code(src);
    let fns = functions(&view);
    let mut out = BTreeSet::new();
    for t in tokens {
        let mut from = 0;
        while let Some(found) = view[from..].find(t) {
            let at = from + found;
            from = at + t.len();
            if !alone(&view, at, t) {
                continue;
            }
            let inner = fns
                .iter()
                .filter(|(_, open, close)| *open < at && at < *close)
                .max_by_key(|(_, open, _)| *open)
                .map_or("", |(name, _, _)| name.as_str());
            out.insert((inner.to_owned(), (*t).to_owned()));
        }
    }
    out
}

/// The files the mods code is in, relative to the crate.
fn mods_files() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![root.join("src/mods.rs")];
    rust_files(&root.join("src/mods"), &mut files);
    rust_files(&root.join("src/engines"), &mut files);
    files.sort();
    files
        .into_iter()
        .map(|f| {
            let rel = f
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (rel, fs::read_to_string(&f).unwrap())
        })
        .collect()
}

#[test]
fn the_mods_code_writes_only_astras_own_files_and_logs_nothing() {
    let files = mods_files();
    assert!(
        files.iter().any(|(f, _)| f == "src/engines/mods/eval.rs")
            && files.iter().any(|(f, _)| f == "src/mods/builtin.rs"),
        "{:?}",
        files.iter().map(|(f, _)| f).collect::<Vec<_>>()
    );
    let mut writes = BTreeSet::new();
    let mut logs = Vec::new();
    for (file, src) in &files {
        for (f, t) in calls(src, WRITES) {
            writes.insert((file.clone(), f, t));
        }
        for (f, t) in calls(src, LOGS) {
            logs.push(format!("{file}: {f}: {t}"));
        }
    }
    let allowed: BTreeSet<(String, String, String)> = ALLOWED
        .iter()
        .map(|(a, b, c)| ((*a).to_owned(), (*b).to_owned(), (*c).to_owned()))
        .collect();
    let extra: Vec<_> = writes.difference(&allowed).collect();
    assert!(
        extra.is_empty(),
        "the mods code writes where it may not (engine-descriptor-v1.md §7.10): {extra:#?}"
    );
    // Every allowed write is still found: the scan sees what it is meant to see.
    let missing: Vec<_> = allowed.difference(&writes).collect();
    assert!(
        missing.is_empty(),
        "an allowed write is no longer where ALLOWED says: {missing:#?}"
    );
    assert!(logs.is_empty(), "the mods code logs (§7.10): {logs:#?}");
}

/// The scan itself: a write or a log written as code is found, in the function it is in; the
/// same words in a comment, a doc, a string or a test module are not.
#[test]
fn the_scan_finds_calls_and_never_prose() {
    let src = r####"
        /// fs::write(root, b"") in a doc
        fn discover(root: &Path) {
            // std::fs::remove_file(x) in a comment
            let m = "fs::write(here) in a message";
            let c = '{';
            fs::write(root.join("x"), b"").ok();
            if true { std::fs::rename(a, b).ok(); }
            let f = std::fs::OpenOptions::new().append(true).open(p);
            eprintln!("found {m}");
        }
        fn quiet() -> &'static str { "std::fs::remove_dir_all(x) and println!(y) in a message" }
        fn my_rename(x: u8) -> u8 { x }
        fn inner() { let _ = my_rename(1); custom(libc::O_NOFOLLOW | libc::O_WRONLY); }
        #[cfg(test)]
        mod tests {
            fn t() { std::fs::remove_dir_all(d).unwrap(); println!("x"); }
        }
    "####;
    let pairs =
        |tokens: &[&str]| -> Vec<(String, String)> { calls(src, tokens).into_iter().collect() };
    assert_eq!(
        pairs(WRITES),
        [
            ("discover".to_owned(), ".append(".to_owned()),
            ("discover".to_owned(), "fs::write(".to_owned()),
            ("discover".to_owned(), "rename(".to_owned()),
            ("inner".to_owned(), "O_WRONLY".to_owned()),
        ]
    );
    assert_eq!(
        pairs(LOGS),
        [("discover".to_owned(), "eprintln!".to_owned())]
    );
}
