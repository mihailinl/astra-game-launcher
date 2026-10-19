// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Computes `SOURCE_TREE_SHA256`: a hash of this crate's own source tree, exposed to the lib as
//! `env!("SOURCE_TREE_SHA256")` (see `src/lib.rs`). This is what makes a build — Astra's pinned
//! git dependency, or a checkout of a release tag — provably built from a given set of bytes:
//! the CLI's `--version` prints it, Astra's About screen and release notes show it, the GitHub
//! release for a tag publishes it, and anyone can recompute it from a clone.
//!
//! **Algorithm — keep this comment, the README's one-liner and `scripts/tree-hash.sh` in sync:**
//! walk every REGULAR file under `CARGO_MANIFEST_DIR`, excluding any file or directory named
//! `.git`, `.cargo-ok` or `target` (at any depth — nothing else is excluded); for each file,
//! sorted by its `/`-separated relative path as raw bytes, hash the line
//! `<sha256 hex of contents>  <relative path>\n` (two spaces, like `sha256sum`'s own output) into
//! one running SHA-256; the tree hash is that digest's hex. A symlink anywhere in the tree, or a
//! non-UTF-8 path, makes the build fail outright rather than guess at what it should mean.
//!
//! **Staying correct for local edits:** cargo's `rerun-if-changed` on a directory re-scans it
//! recursively, so watching each top-level entry that existed at the last build (every one but
//! `target/`, which must never be watched — its own output touches itself on every build, which
//! would retrigger this script forever) covers every edit, add or delete *below* those entries.
//! The one case it cannot see: a brand-new file or directory dropped directly at the crate root
//! (nothing was watching the spot it appears in, because nothing was there to watch). That is
//! rare — day to day work edits files inside `src/`, `tests/`, `docs/`, `examples/`, all already
//! watched — but when it happens, `cargo clean -p astra-game-launcher` (or `touch build.rs`)
//! forces the next build to re-scan from scratch.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Names excluded at any depth. Nothing else is excluded — see the module doc.
const EXCLUDE: &[&str] = &[".git", ".cargo-ok", "target"];

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    for entry in fs::read_dir(&manifest_dir).expect("read CARGO_MANIFEST_DIR") {
        let entry = entry.expect("read CARGO_MANIFEST_DIR entry");
        let name = entry.file_name();
        let name = name.to_str().expect("non-UTF-8 top-level entry name");
        if EXCLUDE.contains(&name) {
            continue;
        }
        println!("cargo:rerun-if-changed={}", entry.path().display());
    }

    let mut files = Vec::new();
    collect_files(&manifest_dir, &manifest_dir, &mut files);
    files.sort();

    let mut hasher = Sha256::new();
    for rel in &files {
        let abs = manifest_dir.join(rel);
        let contents = fs::read(&abs).unwrap_or_else(|e| panic!("read {abs:?}: {e}"));
        let file_hash = hex::encode(Sha256::digest(&contents));
        hasher.update(file_hash.as_bytes());
        hasher.update(b"  ");
        hasher.update(rel.as_bytes());
        hasher.update(b"\n");
    }
    let tree_hash = hex::encode(hasher.finalize());

    println!("cargo:rustc-env=SOURCE_TREE_SHA256={tree_hash}");
}

/// Recursively collects every regular file under `dir`, as `/`-separated paths relative to
/// `root`, skipping [`EXCLUDE`] at any depth. Panics on a symlink or a non-UTF-8 name.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let entry = entry.unwrap_or_else(|e| panic!("read_dir entry in {dir:?}: {e}"));
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(n) => n,
            None => panic!("SOURCE_TREE_SHA256: non-UTF-8 path under {dir:?}: {name:?}"),
        };
        if EXCLUDE.contains(&name) {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|e| panic!("file_type {path:?}: {e}"));
        if file_type.is_symlink() {
            panic!("SOURCE_TREE_SHA256: refusing a symlink in the source tree: {path:?}");
        } else if file_type.is_dir() {
            collect_files(root, &path, out);
        } else if file_type.is_file() {
            let rel = path.strip_prefix(root).expect("path under root");
            let rel = rel
                .components()
                .map(|c| {
                    c.as_os_str().to_str().unwrap_or_else(|| {
                        panic!("SOURCE_TREE_SHA256: non-UTF-8 path component in {path:?}")
                    })
                })
                .collect::<Vec<_>>()
                .join("/");
            out.push(rel);
        }
        // Neither a symlink, dir nor regular file (a socket, fifo, …): not a source file, skip.
    }
}
