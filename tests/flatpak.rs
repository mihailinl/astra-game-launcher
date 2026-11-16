// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Play through Flathub's Steam (0.5.3): the plan is `flatpak run com.valvesoftware.Steam
//! -applaunch …`, and `launch` grants Steam's sandbox read/write access to exactly
//! `<profile>/BepInEx` — never the profile, whose root holds the ledger, the backups and the
//! grant's own record — in the user's Flatpak overrides, right before the spawn, after reading
//! Steam's permissions; `uninstall` takes out exactly what the grant added. Every overrides file
//! here is a temporary one and every `flatpak` a script: nothing touches this machine's Flatpak.

#![cfg(unix)]

mod common;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use astra_game_launcher::{
    FLATPAK_GRANT_FILE_NAME, FLATPAK_REVOKE_FAILED, FlatpakGrant, LEDGER_FILE_NAME, LauncherError,
    MODS_CHOICE_FILE_NAME, PLAY_ARM_FILE_NAME, Platform, ProcessPlan, SANDBOX_FOLDER,
    STEAM_FLATPAK_APP, Spawn, SteamCommand, UninstallReport, codes, flatpak::grant_sandbox_access,
    launch_with, prepare_launch, uninstall_with,
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
    /// The test runner installed, and a `flatpak` that answers `info --show-permissions` with
    /// `permissions`.
    fn installed(permissions: &str) -> World {
        World::installed_with(&runner_manifest(""), permissions)
    }

    /// The package of `manifest` installed.
    fn installed_with(manifest: &str, permissions: &str) -> World {
        let w = World {
            game: Tmp::new("fp-game"),
            profiles: Tmp::new("fp-profiles"),
            bin: Tmp::new("fp-bin"),
            data: Tmp::new("fp-data"),
        };
        unity_game(w.game.path(), "Game");
        let pkg = open(&runner_zip(manifest)).unwrap();
        plan_and_install(&pkg, &w.target(), &w.profile());
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = info ] && [ \"$2\" = --show-permissions ] && [ \"$3\" = {STEAM_FLATPAK_APP} ]; then\ncat <<'EOF'\n{permissions}EOF\nexit 0\nfi\nexit 1\n"
        );
        executable(&w.flatpak(), &script);
        w
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    fn granted(&self) -> PathBuf {
        self.profile().join(SANDBOX_FOLDER)
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
        format!("{}:rw", self.granted().display())
    }

    fn prepare(&self) -> Result<ProcessPlan, LauncherError> {
        prepare_launch(
            &self.profile(),
            &self.target(),
            &vars(),
            Some(&self.steam()),
            &no_markers(),
        )
    }

    fn plan(&self) -> ProcessPlan {
        self.prepare().unwrap()
    }

    fn launch(&self, plan: &ProcessPlan, spy: &Spy) -> Result<(), LauncherError> {
        launch_with(plan, spy, &no_markers(), Some(&self.overrides()))
    }

    fn uninstall(&self) -> UninstallReport {
        uninstall_with(&self.profile(), &self.target(), Some(&self.overrides())).unwrap()
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
/// com.valvesoftware.Steam`, and only it carries a sandbox grant: `<profile>/BepInEx`.
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
    assert_eq!(flat.sandbox_grant.as_deref(), Some(w.granted().as_path()));
    assert_eq!(native.program, PathBuf::from("/opt/fake/steam"));
    assert_eq!(native.args[..2], ["-applaunch", &APPID.to_string()]);
    assert_eq!(native.sandbox_grant, None);
    // Preparing grants nothing.
    assert_eq!(w.overrides_text(), None);
    assert_eq!(w.record(), None);
}

/// The grant is `<profile>/BepInEx` and nothing above it: the ledger, the backups, the grant's
/// own record, the play-arm and the mods choice — every file the launcher trusts outside the
/// sandbox — are outside the folder granted, and so is the profile itself.
///
/// Mutation (watched red): `grant_sandbox_access` granting the profile instead of its `BepInEx`.
#[test]
fn the_grant_is_bepinex_alone_never_the_profiles_root() {
    let w = World::installed(PERMISSIONS);
    w.launch(&w.plan(), &Spy::on(&w)).unwrap();
    let record = w.record().unwrap();
    assert_eq!(record.entry, w.entry());
    let granted = PathBuf::from(record.entry.strip_suffix(":rw").unwrap());
    assert_eq!(granted, w.granted());
    assert!(granted.is_dir());
    for name in [
        LEDGER_FILE_NAME,
        FLATPAK_GRANT_FILE_NAME,
        PLAY_ARM_FILE_NAME,
        MODS_CHOICE_FILE_NAME,
        "astra-launcher-backup",
        "astra-launcher-ledger.lock",
    ] {
        let p = w.profile().join(name);
        assert!(!p.starts_with(&granted), "{name} inside the grant");
    }
    assert!(w.profile().join(LEDGER_FILE_NAME).is_file());
    assert!(w.profile().join(FLATPAK_GRANT_FILE_NAME).is_file());
    assert!(!w.profile().starts_with(&granted));
    // Everything the game is handed from the profile is inside it.
    let plan = w.plan();
    let profile = w.profile().display().to_string();
    for arg in plan.args.iter().filter(|a| a.contains(&profile)) {
        assert!(
            arg.contains(&format!("{profile}/{SANDBOX_FOLDER}/")),
            "{arg}"
        );
    }
}

/// A plan that hands the game a profile path outside `BepInEx` — a `[[config_writes]]` file at the
/// profile's root, a launch argument naming another folder of it, the profile itself — is refused
/// rather than granted more, before anything is written. The native Steam plays it as before.
///
/// Mutation (watched red): `sandbox_layout` without its `[[config_writes]]` check; without its
/// argument check.
#[test]
fn a_plan_that_needs_more_than_bepinex_is_refused() {
    let at_root = runner_manifest("").replace(
        "file = \"${profile}/BepInEx/config/astra.cfg\"",
        "file = \"${profile}/astra.cfg\"",
    );
    let elsewhere = runner_manifest("").replace(
        "\"${profile}/BepInEx/core/BepInEx.Preloader.dll\"]",
        "\"${profile}/BepInEx/core/BepInEx.Preloader.dll\", \"--doorstop-clr-corlib-dir\", \"${profile}/dotnet\"]",
    );
    let the_profile = runner_manifest("").replace(
        "\"${profile}/BepInEx/core/BepInEx.Preloader.dll\"]",
        "\"${profile}/BepInEx/core/BepInEx.Preloader.dll\", \"--root\", \"${profile}\"]",
    );
    for (manifest, path) in [
        (&at_root, "${profile}/astra.cfg".to_owned()),
        (&elsewhere, "dotnet".to_owned()),
        (&the_profile, String::new()),
    ] {
        assert_ne!(manifest, &runner_manifest(""));
        let w = World::installed_with(manifest, PERMISSIONS);
        let before = snapshot(&w.profile());
        let e = w.prepare().unwrap_err();
        unusable(&e, "layout");
        assert!(e.params["path"].ends_with(&path), "{e:?}");
        assert!(
            snapshot(&w.profile()) == before,
            "a refused Play wrote nothing"
        );
        prepare_launch(
            &w.profile(),
            &w.target(),
            &vars(),
            fake_steam(),
            &no_markers(),
        )
        .unwrap();
    }
}

/// The grant lands before the spawn, adds exactly `<profile>/BepInEx:rw` to the user's own
/// overrides, is recorded, is not added twice, and uninstall takes out exactly what it added: the
/// file comes back byte for byte, whatever shape the user's list had — closed, not closed by a
/// `;`, empty, ending in a space, with CRLF lines, in a group with no list, in a file with no
/// `[Context]`, no newline at the end.
///
/// Mutation (watched red): `launch_with` without its grant; `uninstall` without
/// `revoke_recorded`; `remove_entry` ignoring the recorded `separator`.
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
    let record = w.record().unwrap();
    assert_eq!(
        (
            record.app.as_str(),
            record.entry.clone(),
            record.overrides.clone(),
            record.preexisting
        ),
        (STEAM_FLATPAK_APP, w.entry(), w.overrides(), false)
    );
    // Again: nothing added twice, and still ours.
    w.launch(&w.plan(), &spy).unwrap();
    assert_eq!(w.overrides_text().as_deref(), Some(granted.as_str()));
    assert!(!w.record().unwrap().preexisting);
    let r = w.uninstall();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(w.overrides_text().as_deref(), Some(USERS));
    assert_eq!(w.record(), None);

    for seed in [
        "[Context]\nfilesystems=/home/u/Games:ro\n",
        "[Context]\nfilesystems=\n",
        "[Context]\nfilesystems=/home/u/Games:ro; \n",
        "[Context]\r\nfilesystems=/a;/b\r\nshared=ipc;\r\n",
        "[Context]\nshared=network;\n",
        "[Session Bus Policy]\norg.kde.StatusNotifierWatcher=talk",
        "",
    ] {
        let w = World::installed(PERMISSIONS);
        write(&w.overrides(), seed.as_bytes());
        w.launch(&w.plan(), &Spy::on(&w)).unwrap();
        assert!(
            w.overrides_text()
                .unwrap()
                .contains(&format!("{};", w.entry()))
        );
        let r = w.uninstall();
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(w.overrides_text().as_deref(), Some(seed), "{seed:?}");
    }
}

/// With no overrides file, the grant creates it (and its folders), and uninstall deletes it again.
///
/// Mutation (watched red): `revoke_recorded` never deleting a file the grant created.
#[test]
fn an_absent_overrides_file_is_created_and_deleted_again() {
    let w = World::installed(PERMISSIONS);
    w.launch(&w.plan(), &Spy::on(&w)).unwrap();
    assert_eq!(
        w.overrides_text(),
        Some(format!("[Context]\nfilesystems={};\n", w.entry()))
    );
    assert!(w.uninstall().warnings.is_empty());
    assert!(!w.overrides().exists());
}

/// A folder the user listed already, in any mode, is theirs: nothing is written, and uninstall
/// takes nothing out.
#[test]
fn an_already_listed_folder_is_the_users() {
    let w = World::installed(PERMISSIONS);
    let seed = format!(
        "[Context]\nfilesystems={}:ro;\nshared=network;\n",
        w.granted().display()
    );
    write(&w.overrides(), seed.as_bytes());
    w.launch(&w.plan(), &Spy::on(&w)).unwrap();
    assert_eq!(w.overrides_text().as_deref(), Some(seed.as_str()));
    assert!(w.record().unwrap().preexisting);
    let r = w.uninstall();
    assert!(r.warnings.is_empty());
    assert_eq!(w.overrides_text().as_deref(), Some(seed.as_str()));
    assert_eq!(w.record(), None);
}

/// A folder the user denies Steam (`!<folder>`) is refused, never overridden.
#[test]
fn a_denied_folder_is_refused() {
    let w = World::installed(PERMISSIONS);
    let seed = format!("[Context]\nfilesystems=!{};\n", w.granted().display());
    write(&w.overrides(), seed.as_bytes());
    let spy = Spy::on(&w);
    unusable(&w.launch(&w.plan(), &spy).unwrap_err(), "denied");
    assert!(spy.seen.borrow().is_empty());
    assert_eq!(w.overrides_text().as_deref(), Some(seed.as_str()));
}

/// Steam's sandbox without `network` or `ipc` (or a `flatpak` that cannot say) refuses before
/// anything is written: no grant, no record, no play-arm, no spawn.
///
/// Mutation (watched red): `grant_sandbox_access` checking the permissions after the write.
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

/// Only the folder `prepare_launch` resolved is granted: a plan whose `sandbox_grant` was changed
/// (to the profile's root, or anything else) is refused, and one read back (no gate) is spawned as
/// it is, granting nothing.
#[test]
fn only_the_prepared_folder_is_granted() {
    let w = World::installed(PERMISSIONS);
    for other in [w.profile(), w.profiles.path().to_path_buf()] {
        let mut plan = w.plan();
        plan.sandbox_grant = Some(other);
        unusable(&w.launch(&plan, &Spy::on(&w)).unwrap_err(), "path");
    }
    assert_eq!(w.overrides_text(), None);

    let read_back: ProcessPlan =
        serde_json::from_str(&serde_json::to_string(&w.plan()).unwrap()).unwrap();
    assert!(read_back.sandbox_grant.is_some());
    let spy = Spy::on(&w);
    w.launch(&read_back, &spy).unwrap();
    assert_eq!(spy.seen.borrow().as_slice(), [None]);
    assert_eq!(w.record(), None);
}

/// The grant names a folder by its canonical spelling alone: a profile reached through a link, a
/// `BepInEx` that is a link, a character a GKeyFile list would need escaped, a profile that is a
/// file, `/` and the home folder are refused.
///
/// Mutation (watched red): `checked_folder` without its canonical comparison.
#[test]
fn a_linked_or_escaping_folder_is_refused() {
    let w = World::installed(PERMISSIONS);
    let profile_with = |name: &str| {
        let p = w.profiles.join(name);
        fs::create_dir_all(p.join(SANDBOX_FOLDER)).unwrap();
        p
    };
    let real = profile_with("real");
    let link = w.profiles.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let grant = |p: &Path| grant_sandbox_access(p, &w.overrides(), &w.flatpak());
    unusable(&grant(&link).unwrap_err(), "path");
    unusable(&grant(&link.join(".")).unwrap_err(), "path");
    // A `BepInEx` that is a link to a folder elsewhere.
    let linked_folder = w.profiles.join("linked-folder");
    fs::create_dir_all(&linked_folder).unwrap();
    std::os::unix::fs::symlink(&real, linked_folder.join(SANDBOX_FOLDER)).unwrap();
    unusable(&grant(&linked_folder).unwrap_err(), "path");
    for name in ["a;b", "a=b", "a[b]", "a\\b", "a:b", "a\nb"] {
        unusable(&grant(&profile_with(name)).unwrap_err(), "path");
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
    // The plain profile next to them is granted its `BepInEx`, recorded beside it.
    grant(&real).unwrap();
    assert!(
        w.overrides_text()
            .unwrap()
            .contains(&format!("{}/{SANDBOX_FOLDER}:rw;", real.display()))
    );
    assert!(real.join(FLATPAK_GRANT_FILE_NAME).is_file());
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
    let r = uninstall_with(&w.profile(), &w.target(), Some(&w.overrides()));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let r = r.unwrap();
    assert_eq!(r.warnings, [FLATPAK_REVOKE_FAILED]);
    assert!(w.overrides_text().unwrap().contains(&w.entry()));
    assert!(w.record().is_some());
}

/// Revoke edits the caller's overrides file and no other: a record that names another file (an
/// edited record, a decoy holding the same entry) edits nothing, and neither does an uninstall
/// told of another file than the one granted into.
///
/// Mutation (watched red): `revoke_recorded` without comparing the record's file to the caller's.
#[test]
fn revoke_edits_only_the_callers_overrides_file() {
    let w = World::installed(PERMISSIONS);
    w.launch(&w.plan(), &Spy::on(&w)).unwrap();
    let granted = w.overrides_text().unwrap();
    // A decoy the record is edited to name, holding the same entry.
    let decoy = w.data.join(&format!("decoy/overrides/{STEAM_FLATPAK_APP}"));
    write(&decoy, granted.as_bytes());
    let mut record = w.record().unwrap();
    record.overrides = decoy.clone();
    write(
        &w.profile().join(FLATPAK_GRANT_FILE_NAME),
        &serde_json::to_vec(&record).unwrap(),
    );
    let r = w.uninstall();
    assert_eq!(r.warnings, [FLATPAK_REVOKE_FAILED]);
    assert_eq!(
        fs::read_to_string(&decoy).unwrap(),
        granted,
        "the decoy untouched"
    );
    assert_eq!(w.overrides_text().unwrap(), granted);
    // Told of another file than the one granted into: nothing edited either.
    let w = World::installed(PERMISSIONS);
    w.launch(&w.plan(), &Spy::on(&w)).unwrap();
    let granted = w.overrides_text().unwrap();
    let r = uninstall_with(&w.profile(), &w.target(), Some(&decoy)).unwrap();
    assert_eq!(r.warnings, [FLATPAK_REVOKE_FAILED]);
    assert_eq!(w.overrides_text().unwrap(), granted);
}
