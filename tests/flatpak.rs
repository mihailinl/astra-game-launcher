// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Play through Flathub's Steam (0.5.3): the plan is `flatpak run com.valvesoftware.Steam
//! -applaunch …`, and `launch` grants Steam's sandbox read/write access to exactly the profile
//! folder in the user's Flatpak overrides, right before the spawn, after reading Steam's
//! permissions; `uninstall` takes exactly that entry out again. Every overrides file here is a
//! temporary one and every `flatpak` a script: nothing touches this machine's Flatpak.

#![cfg(unix)]

mod common;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use astra_game_launcher::{
    FLATPAK_GRANT_FILE_NAME, FLATPAK_REVOKE_FAILED, FlatpakGrant, LauncherError,
    PLAY_ARM_FILE_NAME, Platform, ProcessPlan, STEAM_FLATPAK_APP, Spawn, SteamCommand, codes,
    flatpak::grant_profile_access, launch_with, prepare_launch, uninstall,
};
use common::*;

const APPID: u32 = 4_000_777;

/// Flatpak's own permissions for Steam as Flathub ships it.
const PERMISSIONS: &str = "[Context]\nshared=network;ipc;\nsockets=x11;wayland;pulseaudio;\ndevices=all;\nfilesystems=xdg-music:ro;xdg-pictures:ro;\n\n[Session Bus Policy]\norg.freedesktop.Notifications=talk\n";

/// A user's own overrides for Steam, which must come out of every grant and revoke as they went in.
const USERS: &str = "[Context]\nfilesystems=/home/u/Games:ro;!/home/u/.ssh;\n\n[Session Bus Policy]\norg.kde.StatusNotifierWatcher=talk\n";

struct World {
    game: Tmp,
    profiles: Tmp,
    bin: Tmp,
    data: Tmp,
}

impl World {
    /// An installed integration, and a `flatpak` that answers `info --show-permissions` with
    /// `permissions`.
    fn installed(permissions: &str) -> World {
        let w = World {
            game: Tmp::new("fp-game"),
            profiles: Tmp::new("fp-profiles"),
            bin: Tmp::new("fp-bin"),
            data: Tmp::new("fp-data"),
        };
        unity_game(w.game.path(), "Game");
        plan_and_install(&runner(), &w.target(), &w.profile());
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = info ] && [ \"$2\" = --show-permissions ] && [ \"$3\" = {STEAM_FLATPAK_APP} ]; then\ncat <<'EOF'\n{permissions}EOF\nexit 0\nfi\nexit 1\n"
        );
        executable(&w.flatpak(), &script);
        w
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn target(&self) -> astra_game_launcher::GameTarget {
        target(self.game.path(), "Game.exe", Some(APPID), Platform::Windows)
    }

    fn flatpak(&self) -> PathBuf {
        self.bin.join("flatpak")
    }

    fn steam(&self) -> SteamCommand {
        SteamCommand::flatpak(self.flatpak())
    }

    /// The user's overrides file for Steam, below a data folder no Flatpak reads.
    fn overrides(&self) -> PathBuf {
        self.data
            .join(&format!("flatpak/overrides/{STEAM_FLATPAK_APP}"))
    }

    fn overrides_text(&self) -> Option<String> {
        fs::read_to_string(self.overrides()).ok()
    }

    fn entry(&self) -> String {
        format!("{}:rw", self.profile().display())
    }

    fn plan(&self) -> ProcessPlan {
        prepare_launch(
            &self.profile(),
            &self.target(),
            &vars(),
            Some(&self.steam()),
            &no_markers(),
        )
        .unwrap()
    }

    fn launch(&self, plan: &ProcessPlan, spy: &Spy) -> Result<(), LauncherError> {
        launch_with(plan, spy, &no_markers(), Some(&self.overrides()))
    }

    fn record(&self) -> Option<FlatpakGrant> {
        let bytes = fs::read(self.profile().join(FLATPAK_GRANT_FILE_NAME)).ok()?;
        Some(serde_json::from_slice(&bytes).unwrap())
    }
}

fn executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, text.as_bytes());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A spawner that records, for every spawn, what the overrides file held at that moment.
struct Spy {
    overrides: PathBuf,
    seen: RefCell<Vec<Option<String>>>,
}

impl Spy {
    fn on(w: &World) -> Spy {
        Spy {
            overrides: w.overrides(),
            seen: RefCell::new(Vec::new()),
        }
    }
}

impl Spawn for Spy {
    fn spawn(&self, _: &ProcessPlan) -> Result<(), LauncherError> {
        self.seen
            .borrow_mut()
            .push(fs::read_to_string(&self.overrides).ok());
        Ok(())
    }
}

fn unusable(e: &LauncherError, reason: &str) {
    assert_eq!(e.code, codes::STEAM_FLATPAK_UNUSABLE, "{e:?}");
    assert_eq!(e.params["reason"], reason, "{e:?}");
}

/// The plan through Flathub's Steam is the native plan behind `flatpak run
/// com.valvesoftware.Steam`, and only it carries a sandbox grant: the profile, canonical.
///
/// Mutation (watched red): `prepare_launch_with` setting `sandbox_grant` for every Steam.
#[test]
fn the_flatpak_plan_is_the_native_one_behind_flatpak_run() {
    let w = World::installed(PERMISSIONS);
    let flat = w.plan();
    let native = prepare_launch(
        &w.profile(),
        &w.target(),
        &vars(),
        fake_steam(),
        &no_markers(),
    )
    .unwrap();
    assert_eq!(flat.program, w.flatpak());
    assert_eq!(
        flat.args[..4],
        ["run", STEAM_FLATPAK_APP, "-applaunch", &APPID.to_string()]
    );
    assert_eq!(flat.args[2..], native.args[..]);
    assert_eq!(
        flat.sandbox_grant.as_deref(),
        Some(fs::canonicalize(w.profile()).unwrap().as_path())
    );
    assert_eq!(native.program, PathBuf::from("/opt/fake/steam"));
    assert_eq!(native.args[..2], ["-applaunch", &APPID.to_string()]);
    assert_eq!(native.sandbox_grant, None);
    // Preparing grants nothing.
    assert_eq!(w.overrides_text(), None);
    assert_eq!(w.record(), None);
}

/// The grant lands before the spawn, adds exactly `<profile>:rw` to the user's own overrides, is
/// recorded, is not added twice, and uninstall takes exactly it out again: the file comes back
/// byte for byte.
///
/// Mutation (watched red): `launch_with` without its grant; `uninstall` without
/// `revoke_recorded`.
#[test]
fn a_users_overrides_survive_play_and_uninstall_byte_for_byte() {
    let w = World::installed(PERMISSIONS);
    write(&w.overrides(), USERS.as_bytes());
    let spy = Spy::on(&w);
    let plan = w.plan();
    w.launch(&plan, &spy).unwrap();
    let granted = USERS.replacen(
        "!/home/u/.ssh;",
        &format!("!/home/u/.ssh;{};", w.entry()),
        1,
    );
    assert_eq!(
        spy.seen.borrow().as_slice(),
        [Some(granted.clone())],
        "granted before the spawn"
    );
    assert_eq!(w.overrides_text().as_deref(), Some(granted.as_str()));
    assert_eq!(
        w.record(),
        Some(FlatpakGrant {
            app: STEAM_FLATPAK_APP.to_owned(),
            entry: w.entry(),
            overrides: w.overrides(),
            preexisting: false,
        })
    );
    // Again: nothing added twice, and still ours.
    w.launch(&w.plan(), &spy).unwrap();
    assert_eq!(w.overrides_text().as_deref(), Some(granted.as_str()));
    assert!(!w.record().unwrap().preexisting);

    let r = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(w.overrides_text().as_deref(), Some(USERS));
    assert_eq!(w.record(), None);
}

/// With no overrides file, the grant creates it (and its folders) and uninstall leaves the group
/// with no list.
#[test]
fn an_absent_overrides_file_is_created() {
    let w = World::installed(PERMISSIONS);
    w.launch(&w.plan(), &Spy::on(&w)).unwrap();
    assert_eq!(
        w.overrides_text(),
        Some(format!("[Context]\nfilesystems={};\n", w.entry()))
    );
    uninstall(&w.profile(), &w.target()).unwrap();
    assert_eq!(w.overrides_text().as_deref(), Some("[Context]\n"));
}

/// A profile the user listed already, in any mode, is theirs: nothing is written, and uninstall
/// takes nothing out.
#[test]
fn an_already_listed_profile_is_the_users() {
    let w = World::installed(PERMISSIONS);
    let seed = format!(
        "[Context]\nfilesystems={}:ro;\nshared=network;\n",
        w.profile().display()
    );
    write(&w.overrides(), seed.as_bytes());
    w.launch(&w.plan(), &Spy::on(&w)).unwrap();
    assert_eq!(w.overrides_text().as_deref(), Some(seed.as_str()));
    assert!(w.record().unwrap().preexisting);
    let r = uninstall(&w.profile(), &w.target()).unwrap();
    assert!(r.warnings.is_empty());
    assert_eq!(w.overrides_text().as_deref(), Some(seed.as_str()));
    assert_eq!(w.record(), None);
}

/// A profile the user denies Steam (`!<profile>`) is refused, never overridden.
#[test]
fn a_denied_profile_is_refused() {
    let w = World::installed(PERMISSIONS);
    let seed = format!("[Context]\nfilesystems=!{};\n", w.profile().display());
    write(&w.overrides(), seed.as_bytes());
    let spy = Spy::on(&w);
    unusable(&w.launch(&w.plan(), &spy).unwrap_err(), "denied");
    assert!(spy.seen.borrow().is_empty());
    assert_eq!(w.overrides_text().as_deref(), Some(seed.as_str()));
}

/// Steam's sandbox without `network` or `ipc` (or a `flatpak` that cannot say) refuses before
/// anything is written: no grant, no record, no play-arm, no spawn.
///
/// Mutation (watched red): `grant_profile_access` checking the permissions after the write.
#[test]
fn missing_permissions_refuse_before_anything_is_written() {
    for (shared, missing) in [
        ("network;", Some("ipc")),
        ("ipc;", Some("network")),
        ("", Some("network,ipc")),
    ] {
        let w = World::installed(&format!("[Context]\nshared={shared}\n"));
        let spy = Spy::on(&w);
        let e = w.launch(&w.plan(), &spy).unwrap_err();
        unusable(&e, "permission");
        assert_eq!(e.params.get("missing").map(String::as_str), missing);
        assert!(spy.seen.borrow().is_empty());
        assert_eq!(w.overrides_text(), None);
        assert_eq!(w.record(), None);
        assert!(!w.profile().join(PLAY_ARM_FILE_NAME).exists());
    }
    let w = World::installed("not a key file\n");
    unusable(
        &w.launch(&w.plan(), &Spy::on(&w)).unwrap_err(),
        "unparseable",
    );
    let w = World::installed(PERMISSIONS);
    executable(&w.flatpak(), "#!/bin/sh\nexit 1\n");
    unusable(
        &w.launch(&w.plan(), &Spy::on(&w)).unwrap_err(),
        "flatpak-info",
    );
    assert_eq!(w.overrides_text(), None);
}

/// Only the profile `prepare_launch` resolved is granted: a plan whose `sandbox_grant` was changed
/// is refused, and one read back (no gate) is spawned as it is, granting nothing.
#[test]
fn only_the_prepared_profile_is_granted() {
    let w = World::installed(PERMISSIONS);
    let mut plan = w.plan();
    plan.sandbox_grant = Some(w.profiles.path().to_path_buf());
    unusable(&w.launch(&plan, &Spy::on(&w)).unwrap_err(), "path");
    assert_eq!(w.overrides_text(), None);

    let read_back: ProcessPlan =
        serde_json::from_str(&serde_json::to_string(&w.plan()).unwrap()).unwrap();
    assert!(read_back.sandbox_grant.is_some());
    let spy = Spy::on(&w);
    w.launch(&read_back, &spy).unwrap();
    assert_eq!(spy.seen.borrow().as_slice(), [None]);
    assert_eq!(w.record(), None);
}

/// The grant names a folder by its canonical spelling alone: a link anywhere on the way, a
/// character a GKeyFile list would need escaped, a file, or the home folder are refused.
///
/// Mutation (watched red): `checked_profile` without its canonical comparison.
#[test]
fn a_linked_or_escaping_profile_is_refused() {
    let w = World::installed(PERMISSIONS);
    let real = w.profiles.join("real");
    fs::create_dir_all(&real).unwrap();
    let link = w.profiles.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let grant = |p: &Path| grant_profile_access(p, &w.overrides(), &w.flatpak());
    unusable(&grant(&link).unwrap_err(), "path");
    unusable(&grant(&link.join(".")).unwrap_err(), "path");
    for name in ["a;b", "a=b", "a[b]", "a\\b", "a:b", "a\nb", "trailing "] {
        let p = w.profiles.join(name);
        fs::create_dir_all(&p).unwrap();
        unusable(&grant(&p).unwrap_err(), "path");
    }
    let file = w.profiles.join("file");
    fs::write(&file, b"x").unwrap();
    unusable(&grant(&file).unwrap_err(), "path");
    unusable(&grant(Path::new("/")).unwrap_err(), "path");
    if let Some(home) = std::env::var_os("HOME") {
        unusable(
            &grant(&fs::canonicalize(home).unwrap()).unwrap_err(),
            "path",
        );
    }
    assert_eq!(w.overrides_text(), None, "nothing written");
    // The plain folder next to them is granted.
    grant(&real).unwrap();
    assert!(
        w.overrides_text()
            .unwrap()
            .contains(&format!("{}:rw;", real.display()))
    );
}

/// An uninstall whose revoke cannot write never fails: it says so in a warning, and the record
/// stays.
#[test]
fn a_revoke_that_cannot_write_is_a_warning() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::installed(PERMISSIONS);
    w.launch(&w.plan(), &Spy::on(&w)).unwrap();
    let dir = w.overrides().parent().unwrap().to_path_buf();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::write(dir.join("probe"), b"").is_ok() {
        // Running as root: nothing can be made unwritable here.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let r = uninstall(&w.profile(), &w.target());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let r = r.unwrap();
    assert_eq!(r.warnings, [FLATPAK_REVOKE_FAILED]);
    assert!(w.overrides_text().unwrap().contains(&w.entry()));
    assert!(w.record().is_some());
}
