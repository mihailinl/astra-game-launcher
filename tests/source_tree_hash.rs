// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Proves `build.rs`'s `SOURCE_TREE_SHA256` and the shell recompute in `scripts/tree-hash.sh`
//! (the same commands the README gives as its verification one-liner) agree on THIS checkout.
//! No network, no tag: both sides read the working tree as it stands right now.

use std::process::Command;

#[test]
fn source_tree_hash_matches_the_recompute_script() {
    if Command::new("bash").arg("--version").output().is_err() {
        eprintln!("skipping source_tree_hash_matches_the_recompute_script: no bash on PATH");
        return;
    }
    if Command::new("sha256sum").arg("--version").output().is_err() {
        eprintln!("skipping source_tree_hash_matches_the_recompute_script: no sha256sum on PATH");
        return;
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let script = format!("{manifest_dir}/scripts/tree-hash.sh");
    let out = Command::new("bash")
        .arg(&script)
        .arg(manifest_dir)
        .output()
        .unwrap_or_else(|e| panic!("running {script}: {e}"));
    assert!(
        out.status.success(),
        "{script} exited with {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let recomputed = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(
        recomputed,
        astra_game_launcher::SOURCE_TREE_SHA256,
        "build.rs's SOURCE_TREE_SHA256 and scripts/tree-hash.sh disagree on this checkout; they \
         are supposed to implement the same algorithm (see build.rs's doc comment)"
    );
}
