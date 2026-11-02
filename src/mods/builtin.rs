// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The built-in mod loaders, as data: `builtin/*.mods.toml` beside this file — BepInEx 5 and
//! BepInEx 6 (IL2CPP, exactly `6.0.0-be.788`) for Unity; UE4SS 3, UE4SS 2 and the `~mods` paks
//! for Unreal. They are the launcher's own copy (MPL-2.0) of the specification's built-in
//! fragments (AstraPlugins `testdata/games/descriptors/mods/builtin/*.mods.toml`, at the pin in
//! `docs/ENGINE-DESCRIPTOR.md`), whose files are GPL test data and never built in;
//! `tests/mods_identity.rs` holds each `mods` member for member to the golden's.
//!
//! Each fragment rides on the built-in descriptor it is written for (engine-descriptor-v1.md
//! §7.1: "for a game Pass A placed, only the built-in descriptor's `mods` is consulted"): its
//! `[[mods.loader]]` tables appended to that descriptor's text, and parsed as one descriptor, as
//! the specification's checker reads them. Which built-in a fragment rides on is not written
//! here either: it is the one built-in on which the fragment is a valid `mods` (its loaders'
//! `codes` are that descriptor's own, else `code-undeclared` voids it). A fragment that rides on
//! none, or on two, is a build defect, and every test fails on it.
//!
//! The descriptors parsed here are never asked for detection: Pass A asks [`crate::engines::builtin`],
//! the same texts without `mods`, so the digests and the answers there are what they were (§7.1).

use std::sync::LazyLock;

use crate::engines::mods::Mods;
use crate::engines::{BUILTIN, Descriptor, EngineCode};

/// The fragments, compiled in.
pub(crate) const FRAGMENTS: [&str; 2] = [
    include_str!("builtin/unity.mods.toml"),
    include_str!("builtin/unreal.mods.toml"),
];

/// The `[[mods.loader]]` part of a fragment: from its first loader table on (its `schema` line,
/// which the built-in descriptor already has, and the comments above it left out).
fn loaders_of(fragment: &str) -> &str {
    let at = fragment
        .find("\n[[mods.loader]]")
        .unwrap_or_else(|| panic!("a built-in mods fragment holds no [[mods.loader]]"));
    &fragment[at..]
}

/// Every built-in descriptor that carries a fragment, as one descriptor with its `mods`.
pub(crate) fn descriptors() -> &'static [Descriptor] {
    static SET: LazyLock<Vec<Descriptor>> = LazyLock::new(|| {
        FRAGMENTS
            .iter()
            .map(|fragment| {
                let mut on: Vec<Descriptor> = BUILTIN
                    .iter()
                    .filter_map(|text| {
                        Descriptor::from_toml(&format!("{text}\n{}", loaders_of(fragment)))
                            .ok()
                            .filter(|d| d.mods().is_some())
                    })
                    .collect();
                assert_eq!(
                    on.len(),
                    1,
                    "a built-in mods fragment must ride on exactly one built-in descriptor"
                );
                on.remove(0)
            })
            .collect()
    });
    &SET
}

/// The built-in `mods` for a game a built-in rule placed under `code`: the one of the
/// descriptor that declares `code`. `None` for a code no built-in fragment serves (Godot,
/// `unknown`, a descriptor's own code).
pub(crate) fn for_code(code: &EngineCode) -> Option<&'static Mods> {
    descriptors()
        .iter()
        .find(|d| d.codes().any(|(c, _)| c == code))
        .and_then(Descriptor::mods)
}
