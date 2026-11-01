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

mod common;

use std::fs;
use std::path::Path;

use common::scrape::{literals, rust_files};

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
