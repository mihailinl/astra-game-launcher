// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A ledger is a file on disk, and uninstall and the restore write what its Proton records say
//! into a prefix's `user.reg` (0.5.3). A record is acted on only for the game's own prefix (the
//! caller's target, or Steam's `compatdata/<appid>/pfx` for the game's folder), and only when no
//! field could put a line of its own into `user.reg`. Any other record is never written and is
//! reported (`proton-record-refused`); the uninstall goes on.

mod common;

use std::fs;
use std::path::PathBuf;

use astra_game_launcher::{
    GameTarget, LEDGER_FILE_NAME, PROTON_RECORD_REFUSED, Platform, prepare_launch,
    restore_game_files, uninstall,
};
use common::*;

const GAME: &str = "Some Game";

/// One Proton record of a ledger, as JSON.
type Record = serde_json::Map<String, serde_json::Value>;
const APPID: u32 = 4_000_901;

struct World {
    game: Tmp,
    profiles: Tmp,
    prefix: Tmp,
    /// Another Wine prefix of the user's (`~/.wine`), which no record of this game may touch.
    other: Tmp,
}

impl World {
    /// Installed and played once under Proton, so the ledger holds the `winhttp` record.
    fn played() -> World {
        let w = World {
            game: Tmp::new("pr-game"),
            profiles: Tmp::new("pr-profiles"),
            prefix: Tmp::new("pr-pfx"),
            other: Tmp::new("pr-wine"),
        };
        unity_game(w.game.path(), GAME);
        fs::write(w.prefix.join("user.reg"), USER_REG).unwrap();
        fs::write(w.other.join("user.reg"), USER_REG).unwrap();
        plan_and_install(&runner(), &w.target(), &w.profile());
        prepare_launch(
            &w.profile(),
            &w.target(),
            &vars(),
            fake_steam(),
            &no_markers(),
        )
        .unwrap();
        assert!(w.user_reg().contains("\"winhttp\"=\"native,builtin\""));
        w
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn target(&self) -> GameTarget {
        target(
            self.game.path(),
            &format!("{GAME}.exe"),
            Some(APPID),
            Platform::LinuxProton {
                prefix: Some(self.prefix.path().to_path_buf()),
            },
        )
    }

    fn user_reg(&self) -> String {
        fs::read_to_string(self.prefix.join("user.reg")).unwrap()
    }

    fn other_reg(&self) -> String {
        fs::read_to_string(self.other.join("user.reg")).unwrap()
    }

    /// Rewrites the ledger's (one) Proton record with `edit`, as something that can write the
    /// profile would.
    fn tamper(&self, edit: impl Fn(&mut Record)) {
        let path = self.profile().join(LEDGER_FILE_NAME);
        let mut ledger: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let rec = match &mut ledger["proton"] {
            serde_json::Value::Array(a) => a[0].as_object_mut().unwrap(),
            v => v.as_object_mut().unwrap(),
        };
        edit(rec);
        fs::write(&path, serde_json::to_vec(&ledger).unwrap()).unwrap();
    }
}

/// A record edited to name another prefix (the user's `~/.wine`), with a section, a value and a
/// "previous" that would rewrite a line there, is never written there, by uninstall or by the
/// restore; it is reported, and the uninstall still completes.
///
/// Mutation (watched red): `proton::admissible` without its prefix comparison.
#[test]
fn a_record_naming_another_prefix_is_never_written() {
    for restore in [false, true] {
        let w = World::played();
        let before = w.other_reg();
        w.tamper(|r| {
            r.insert("prefix".into(), w.other.path().display().to_string().into());
            r.insert(
                "section".into(),
                "Software\\Wine\\AppDefaults\\Other.exe\\DllOverrides".into(),
            );
            r.insert("dll".into(), "dxgi".into());
            r.insert("value".into(), "native".into());
            r.insert("previous".into(), "\"planted\"".into());
        });
        let report = if restore {
            restore_game_files(&w.profile(), &w.target()).unwrap()
        } else {
            uninstall(&w.profile(), &w.target()).unwrap()
        };
        assert_eq!(w.other_reg(), before, "another prefix untouched");
        assert!(
            report.warnings.iter().any(|x| x == PROTON_RECORD_REFUSED),
            "{:?}",
            report.warnings
        );
    }
}

/// A record of the game's own prefix whose "previous" (or DLL name) carries a line break or a
/// quote — which `"<dll>"=<previous>` would turn into a line of its own, a `RunOnce` key — is
/// never written; it is reported, and our override is left as it is.
///
/// Mutation (watched red): `proton::admissible` without its field checks.
#[test]
fn a_record_that_would_add_a_line_to_user_reg_is_never_written() {
    let injected = "\"builtin\"\n[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\RunOnce] 1\n\"evil\"=\"cmd\"";
    // (field, value): a line break, a lone carriage return, a quote in the DLL's name.
    let edits = [
        ("previous", injected),
        ("previous", "\"builtin\"\r[RunOnce]"),
        ("dll", "winhttp\"=\"x"),
    ];
    for (field, value) in edits {
        let w = World::played();
        w.tamper(|r| {
            r.insert(field.into(), value.into());
        });
        let before = w.user_reg();
        let report = uninstall(&w.profile(), &w.target()).unwrap();
        assert_eq!(w.user_reg(), before, "user.reg untouched");
        assert!(!w.user_reg().contains("RunOnce"));
        assert!(report.warnings.iter().any(|x| x == PROTON_RECORD_REFUSED));
    }
}

/// An untouched record still restores exactly, with no warning: the check refuses only what it
/// must.
#[test]
fn an_untouched_record_still_restores() {
    let w = World::played();
    let report = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(report.proton_restored);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(w.user_reg(), USER_REG);
}
