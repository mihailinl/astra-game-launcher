// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a game integration may not take for itself: the ONE list (canary K13), held equal to the
//! specification's `spec/games/reserved.yaml` by `tests/goldens.rs` at the pinned AstraPlugins
//! commit (`tests/goldens/astraplugins/PIN`).
//!
//! - **Profile file names** ([`PROFILE_FILE_NAMES`]): the launcher's own files at the top of a
//!   profile. No `[[files]]` destination, `**` expansion or `[[config_writes]]` file may be one
//!   ([`profile_name_is_reserved`], which `paths::is_reserved` reads), whoever publishes it.
//! - **Integration ids** ([`INTEGRATION_ID_PREFIXES`]) and **names**
//!   ([`INTEGRATION_NAME_SUBSTRINGS`]): first-party identity. A community integration whose id
//!   begins with one, or whose name holds one, would read as Astra's own ([`check_identity`]:
//!   `reserved-id`, `reserved-name`). "Astra" inside a name stays allowed: it says what the
//!   integration does.
//! - **Engine codes** ([`engine_codes`]): the built-in engines' and `unknown`, derived from
//!   [`Engine`] and never spelled here, so canary K6 (which does not scan this file) cannot be
//!   walked around. Whether a descriptor may declare a code at all is the wider rule of
//!   [`crate::is_reserved_code`] (look-alikes, family-name and `astra`/`unknown` prefixes,
//!   `ambiguous`), which every one of these codes meets.

use crate::detect::Engine;
use crate::engines::EngineCode;
use crate::engines::descriptor::fold_confusables;
use crate::error::{LauncherError, Result, codes};
use crate::manifest::IntegrationInfo;
use crate::paths;

/// The names no manifest may place at the top of a profile, matched ignoring ASCII case: the
/// ledger, its lock, the backups folder, the play-arm file, the mods choice, and `astra-link.json`
/// (Astra Link's discovery file, A§3.1), reserved before its writer ships so no integration
/// installed meanwhile can hold the name.
pub const PROFILE_FILE_NAMES: &[&str] = &[
    paths::LEDGER_FILE,
    paths::LOCK_FILE,
    paths::BACKUP_DIR,
    paths::PLAY_ARM_FILE,
    paths::MODS_CHOICE_FILE,
    paths::LINK_FILE,
];

/// `[integration] id` prefixes only the first-party publisher may use, matched ignoring ASCII
/// case (an id is ASCII: letters, digits, `.`, `-`, `_`).
pub const INTEGRATION_ID_PREFIXES: &[&str] = &["astra.", "minice."];

/// What no community `[integration] name` may hold, compared after [`fold_name`]: case, marks,
/// look-alike letters and digits, and every character that is not a letter or a digit ignored.
pub const INTEGRATION_NAME_SUBSTRINGS: &[&str] = &["Minice", "Official"];

/// The engine codes no descriptor may declare by name: every built-in engine's, `unknown` last.
pub fn engine_codes() -> Vec<EngineCode> {
    Engine::ALL.iter().map(|e| EngineCode::from(*e)).collect()
}

/// Whether `first`, the first component of a path below a profile, is one of
/// [`PROFILE_FILE_NAMES`] (ignoring ASCII case).
pub fn profile_name_is_reserved(first: &str) -> bool {
    PROFILE_FILE_NAMES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(first))
}

/// Whether `id` begins with one of [`INTEGRATION_ID_PREFIXES`] (ignoring ASCII case).
pub fn id_is_reserved(id: &str) -> bool {
    reserved_prefix(id).is_some()
}

fn reserved_prefix(id: &str) -> Option<&'static str> {
    INTEGRATION_ID_PREFIXES.iter().copied().find(|p| {
        id.get(..p.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(p))
    })
}

/// Whether `name`, folded ([`fold_name`]), holds one of [`INTEGRATION_NAME_SUBSTRINGS`] folded
/// alike: `Official`, `OFFICIAL`, `Οfficial` (a Greek omicron), `Off1cial`, `O f f i c i a l`,
/// `Ｏｆｆｉｃｉａｌ` (full width) and `Offi​cial` (a zero-width space inside) all do.
pub fn integration_name_is_reserved(name: &str) -> bool {
    let folded = fold_name(name);
    INTEGRATION_NAME_SUBSTRINGS
        .iter()
        .any(|w| folded.contains(&fold_name(w)))
}

/// A name as [`integration_name_is_reserved`] compares it: the full-width forms, the
/// mathematical letters and digits and the circled letters read as the ASCII they look like,
/// then [`fold_confusables`] (case, marks, Latin/Cyrillic/Greek look-alikes, `|`/`!`/`$`/`@`),
/// then only letters and digits kept — so no separator, zero-width character or punctuation
/// splits a reserved word — and [`fold_confusables`] once more, for the pairs it reads as one
/// letter (`rn`→`m`, `cl`→`d`) that a dropped character had held apart. An `[integration] name`
/// may hold any script, unlike a descriptor's display name; a compatibility form not listed here
/// (a superscript, a parenthesised letter) is the moderator's to see.
pub fn fold_name(name: &str) -> String {
    let compat: String = name.chars().map(compatibility_ascii).collect();
    let kept: String = fold_confusables(&compat)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    fold_confusables(&kept)
}

/// The ASCII letter or digit a compatibility form stands for, else `c`.
fn compatibility_ascii(c: char) -> char {
    let n = c as u32;
    let letter = |i: u32| -> char {
        let i = i % 52;
        char::from_u32(if i < 26 { 0x41 + i } else { 0x61 + i - 26 }).unwrap_or(c)
    };
    match n {
        // Full width `！`…`～`.
        0xFF01..=0xFF5E => char::from_u32(n - 0xFEE0).unwrap_or(c),
        // Mathematical alphanumeric letters: 13 styles of A-Z a-z, then two of dotless i/j
        // (U+1D6A4-5, read as themselves), Greek (left alone), and five styles of 0-9.
        0x1D400..=0x1D6A3 => letter(n - 0x1D400),
        0x1D7CE..=0x1D7FF => char::from_u32(0x30 + (n - 0x1D7CE) % 10).unwrap_or(c),
        // Circled Latin letters.
        0x24B6..=0x24CF => char::from_u32(0x41 + n - 0x24B6).unwrap_or(c),
        0x24D0..=0x24E9 => char::from_u32(0x61 + n - 0x24D0).unwrap_or(c),
        _ => c,
    }
}

/// A community integration's identity ([`IntegrationInfo`]): `reserved-id{id, prefix}` when its
/// id begins with a first-party prefix, `reserved-name{name}` when its name holds a first-party
/// word. Astra's own integrations are not asked: these names are theirs.
pub fn check_identity(info: &IntegrationInfo) -> Result<()> {
    if let Some(p) = reserved_prefix(&info.id) {
        return Err(LauncherError::new(codes::RESERVED_ID)
            .with("id", info.id.clone())
            .with("prefix", p));
    }
    if integration_name_is_reserved(&info.name) {
        return Err(LauncherError::new(codes::RESERVED_NAME).with("name", info.name.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reserved_engine_code_is_refused_to_descriptors() {
        let all = engine_codes();
        assert_eq!(all.len(), Engine::ALL.len());
        assert!(all.last().is_some_and(|c| c.is_unknown()));
        for c in all {
            assert!(crate::is_reserved_code(c.as_str()), "{c}");
        }
    }

    #[test]
    fn profile_names_ignore_ascii_case() {
        assert!(profile_name_is_reserved("Astra-Link.JSON"));
        assert!(profile_name_is_reserved("MODS-CHOICE.json"));
        assert!(!profile_name_is_reserved("astra-link.json.bak"));
        assert!(!profile_name_is_reserved("BepInEx"));
    }

    #[test]
    fn ids_are_reserved_by_prefix_only() {
        for id in ["astra.peak", "Astra.x", "MINICE.y", "minice.z.w"] {
            assert!(id_is_reserved(id), "{id}");
        }
        for id in [
            "astra",
            "astral.x",
            "someone.astra",
            "astra-x.y",
            "minice",
            "x.minice.",
        ] {
            assert!(!id_is_reserved(id), "{id}");
        }
    }

    #[test]
    fn names_are_reserved_through_every_fold() {
        for n in [
            "Official Astra Companion",
            "OFFICIAL",
            "unofficially yours",
            "\u{39f}fficial",
            "Off1cial",
            "O f f i c i a l",
            "O.f.f.i.c.i.a.l",
            "Offi\u{200b}cial",
            "Offic'ial",
            "Offic\u{2019}ial",
            "\u{ff2f}\u{ff46}\u{ff46}\u{ff49}\u{ff43}\u{ff49}\u{ff41}\u{ff4c}",
            "\u{1d40e}\u{1d41f}\u{1d41f}\u{1d422}\u{1d41c}\u{1d422}\u{1d41a}\u{1d425}",
            "\u{24c4}fficial",
            "By Minice",
            "by m1nice",
            "Mi-ni-ce",
            "\u{41c}inice",
        ] {
            assert!(integration_name_is_reserved(n), "{n:?}");
        }
        for n in [
            "Astra in PEAK",
            "Astra for Source 2",
            "Office tools",
            "Minicraft",
            "Fiction",
            "Astra",
        ] {
            assert!(!integration_name_is_reserved(n), "{n:?}");
        }
    }

    #[test]
    fn identity_refusals_carry_their_codes() {
        let info = |id: &str, name: &str| IntegrationInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            version: "1".to_owned(),
            authors: Vec::new(),
            license: String::new(),
            repo: None,
        };
        assert_eq!(
            check_identity(&info("astra.x", "X")).unwrap_err().code,
            codes::RESERVED_ID
        );
        assert_eq!(
            check_identity(&info("someone.x", "Official X"))
                .unwrap_err()
                .code,
            codes::RESERVED_NAME
        );
        // The id first: one refusal at a time, the same one every time.
        assert_eq!(
            check_identity(&info("minice.x", "Official X"))
                .unwrap_err()
                .code,
            codes::RESERVED_ID
        );
        assert!(check_identity(&info("someone.x", "Astra in X")).is_ok());
    }
}
