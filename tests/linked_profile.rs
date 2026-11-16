// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A profile's `BepInEx` folder is writable by any program in Flathub's Steam's sandbox once Play
//! has granted it (0.5.3), so the launcher's own reads, writes and deletes there must never follow
//! a link that program put in place of a file or a folder: Play's `[[config_writes]]` and
//! uninstall's removal go through the folder held open (`safe_dir`). A swap that races the
//! launcher is made deterministic here by putting the link in place first: the file or the folder
//! IS a link when the launcher reaches it. The file it points at, outside the profile (a stand-in
//! for `~/.ssh/id_ed25519`), must come out untouched, and nothing of it may land in the profile.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use astra_game_launcher::{GameTarget, Platform, codes, prepare_launch, uninstall};
use common::*;

const APPID: u32 = 4_000_955;
const SECRET: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\nnot a real key\n";

struct World {
    game: Tmp,
    profiles: Tmp,
    /// Outside the profile: what a link planted in it points at.
    host: Tmp,
}

impl World {
    fn installed() -> World {
        let w = World {
            game: Tmp::new("lp-game"),
            profiles: Tmp::new("lp-profiles"),
            host: Tmp::new("lp-host"),
        };
        unity_game(w.game.path(), "Game");
        plan_and_install(&runner(), &w.target(), &w.profile());
        fs::write(w.host.join("id_ed25519"), SECRET).unwrap();
        w
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn target(&self) -> GameTarget {
        target(self.game.path(), "Game.exe", Some(APPID), Platform::Windows)
    }

    fn play(&self) -> Result<(), astra_game_launcher::LauncherError> {
        prepare_launch(
            &self.profile(),
            &self.target(),
            &vars(),
            fake_steam(),
            &no_markers(),
        )
        .map(|_| ())
    }

    /// Nothing of the secret anywhere in the profile, and the secret itself as it was.
    fn secret_kept(&self) {
        assert_eq!(fs::read(self.host.join("id_ed25519")).unwrap(), SECRET);
        for (rel, bytes) in snapshot(&self.profile()) {
            // The planted link itself is read through by `snapshot`: only files count.
            if fs::symlink_metadata(self.profile().join(&rel))
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                continue;
            }
            assert!(
                !bytes.windows(SECRET.len()).any(|w| w == SECRET),
                "the secret was copied into the profile as {rel}"
            );
        }
    }
}

/// `BepInEx/config/astra.cfg` is a link to a file outside the profile when Play rewrites it: Play
/// refuses it and reads nothing through it, so the file it names is never copied into the folder
/// the sandbox can read.
///
/// Mutation (watched red): `SafeDir::read` opening the file without `O_NOFOLLOW`.
#[test]
fn a_config_file_swapped_for_a_link_is_never_read_through() {
    let w = World::installed();
    let config = w.profile().join("BepInEx/config");
    fs::create_dir_all(&config).unwrap();
    symlink(w.host.join("id_ed25519"), config.join("astra.cfg")).unwrap();
    let e = w.play().unwrap_err();
    assert_eq!(e.code, codes::PATH_ESCAPES, "{e:?}");
    w.secret_kept();
    assert!(
        fs::symlink_metadata(config.join("astra.cfg"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// `BepInEx/config` is a link to a folder outside the profile when Play writes into it: Play
/// refuses it and writes nothing there.
///
/// Mutation (watched red): `safe_dir`'s walk opening a folder without `O_NOFOLLOW`.
#[test]
fn a_config_folder_swapped_for_a_link_is_never_written_into() {
    let w = World::installed();
    symlink(w.host.path(), w.profile().join("BepInEx/config")).unwrap();
    let e = w.play().unwrap_err();
    assert_eq!(e.code, codes::PATH_ESCAPES, "{e:?}");
    assert_eq!(
        fs::read_dir(w.host.path()).unwrap().count(),
        1,
        "nothing written into the folder the link names"
    );
    w.secret_kept();
}

/// `BepInEx/core` is a link to a folder outside the profile holding files of the very names the
/// ledger records when uninstall removes them: nothing is deleted through the link.
///
/// Mutation (watched red): `uninstall_ledger` removing each file by its path; `safe_dir`'s walk
/// opening a folder without `O_NOFOLLOW`.
#[test]
fn uninstall_never_deletes_through_a_folder_swapped_for_a_link() {
    let w = World::installed();
    let core = w.profile().join("BepInEx/core");
    let elsewhere = w.host.join("core");
    fs::rename(&core, &elsewhere).unwrap();
    symlink(&elsewhere, &core).unwrap();
    let before = snapshot(&elsewhere);
    assert!(before.contains_key("BepInEx.Preloader.dll"), "{before:?}");
    let r = uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(r.profile_files_removed, 0);
    assert!(
        snapshot(&elsewhere) == before,
        "nothing deleted through the link"
    );
    w.secret_kept();
}

/// A Play over a profile whose `BepInEx` holds no link still writes its config, mode `0600` for
/// the one carrying the bridge credential: the walk refuses only what it must.
///
/// Mutation (watched red): `SafeDir::write_atomic` not setting the mode on the open file.
#[test]
fn a_plain_profile_still_plays() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::installed();
    w.play().unwrap();
    let cfg = w.profile().join("BepInEx/config/astra.cfg");
    let text = fs::read_to_string(&cfg).unwrap();
    assert!(text.contains("Token = s3cret"), "{text}");
    assert_eq!(
        fs::metadata(&cfg).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let r = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(r.profile_files_removed >= 2);
    assert!(!w.profile().join("BepInEx/core").exists());
}
