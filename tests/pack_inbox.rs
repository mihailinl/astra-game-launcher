// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `${astra_pack_inbox}` (`src/pack_inbox.rs`; astra-companion `docs/animation-packs.md` §2 and
//! §5 (b)): a game integration's own animation packs go to
//! `<user root>/inbox/packs/<integration id>/`, the root the caller names; a plan without one is
//! refused; the path is jailed; an uninstall writes `<uid tail>.remove` there unless another
//! install holds the pack; launchers before it refuse both the manifest and the ledger.

mod common;

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use astra_game_launcher::{
    Ctx, EngineSet, GameTarget, GiPackage, InstallPlan, Ledger, Manifest, Platform, Provenance,
    UninstallReport, codes, install, installed, plan_install, plan_install_in,
    plan_install_with_user_pack_root, prepare_launch, uninstall,
};
use common::*;

const GAME: &str = "Some Game";
/// The design's example: the archive's name is not its uid.
const PACK: &str = "peak-climb.astrapack";
const UID: &str = "test.unity/climb";
const TOMBSTONE: &str = "climb.remove";
const PACK_ENTRY: &str = "packs/peak-climb.astrapack";

/// A `.astrapack` whose `animpack.toml` (at `prefix`) says `uid`.
fn astrapack_at(prefix: &str, uid: &str) -> Vec<u8> {
    let toml = format!(
        "[animpack]\nname = \"PEAK climbing\"\nkind = \"vrm\"\nuid = \"{uid}\"\nversion = \"1.0.0\"\nowner = \"integration:test.unity\"\ngames = [\"test.unity\"]\n\n[[clips]]\nname = \"climb\"\nfile = \"clips/climb.vrma\"\n"
    );
    let manifest = format!("{prefix}animpack.toml");
    let clip = format!("{prefix}clips/climb.vrma");
    zip(&[
        E::File(&manifest, toml.as_bytes()),
        E::File(&clip, b"glTF fake vrma"),
    ])
}

fn astrapack(uid: &str) -> Vec<u8> {
    astrapack_at("", uid)
}

/// The runner, with `rules` (more `[[files]]`) and its zip's `packs`.
fn with_packs(rules: &str, version: &str, packs: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let m = runner_manifest(rules).replacen(
        "version = \"1.0.0\"",
        &format!("version = \"{version}\""),
        1,
    );
    let mut entries = vec![
        E::File("astra-gi.toml", m.as_bytes()),
        E::File("winhttp.dll", WINHTTP),
        E::File("doorstop_config.ini", DOORSTOP_INI),
        E::File("BepInEx/core/BepInEx.Preloader.dll", PRELOADER),
        E::File("BepInEx/core/BepInEx.dll", CORE_DLL),
    ];
    for (name, bytes) in packs {
        entries.push(E::File(name, bytes));
    }
    zip(&entries)
}

const INTO_INBOX: &str = "\n[[files]]\nfrom = \"packs/**\"\nto = \"${astra_pack_inbox}/\"\n";

fn climber(version: &str) -> GiPackage {
    let pack = astrapack(UID);
    open(&with_packs(INTO_INBOX, version, &[(PACK_ENTRY, pack)])).unwrap()
}

fn refusal(zip: &[u8]) -> astra_game_launcher::LauncherError {
    open(zip).unwrap_err()
}

struct World {
    game: Tmp,
    profiles: Tmp,
    companion: Tmp,
}

impl World {
    fn new() -> World {
        let game = Tmp::new("game");
        unity_game(game.path(), GAME);
        World {
            game,
            profiles: Tmp::new("profiles"),
            companion: Tmp::new("companion"),
        }
    }

    fn profile(&self) -> PathBuf {
        self.profiles.join("p")
    }

    /// The companion's user root: not made yet, as on a machine where it never ran.
    fn root(&self) -> PathBuf {
        self.companion.join("pack")
    }

    fn inbox(&self) -> PathBuf {
        self.root().join("inbox/packs/test.unity")
    }

    fn target(&self) -> GameTarget {
        target(
            self.game.path(),
            &format!("{GAME}.exe"),
            None,
            Platform::Windows,
        )
    }

    fn plan(
        &self,
        pkg: &GiPackage,
        root: Option<&Path>,
    ) -> astra_game_launcher::Result<InstallPlan> {
        plan_install_with_user_pack_root(
            pkg,
            &self.target(),
            &self.profile(),
            &EngineSet::empty(),
            Provenance::FirstParty,
            None,
            root,
        )
    }

    fn install(&self, pkg: &GiPackage) -> Ledger {
        let plan = self.plan(pkg, Some(&self.root())).unwrap();
        install(pkg, &plan, &self.target(), ACCEPT, &Ctx::silent()).unwrap()
    }

    fn uninstall(&self) -> UninstallReport {
        uninstall(&self.profile(), &self.target()).unwrap()
    }

    /// What the companion's engine does with an archive in its inbox: imports it and deletes it.
    fn engine_imports(&self) {
        fs::remove_file(self.inbox().join(PACK)).unwrap();
    }

    /// The inbox's file names, dot-led (the launcher's holds) included.
    fn inbox_names(&self) -> Vec<String> {
        let mut names: Vec<String> = match fs::read_dir(self.inbox()) {
            Ok(d) => d
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        names
    }

    fn holds(&self) -> usize {
        self.inbox_names()
            .iter()
            .filter(|n| n.ends_with(".holds"))
            .count()
    }
}

/// `${astra_pack_inbox}` resolves to `<user root>/inbox/packs/<integration id>/`: the plan says
/// where and which uid, planning writes nothing, the install places the archive there (and a
/// hold beside it), and the ledger records it apart from the profile's files — so Play does not
/// ask for it once the companion has imported (and deleted) it.
#[test]
fn the_destination_resolves_below_the_root_the_caller_names() {
    let w = World::new();
    let pkg = climber("1.0.0");
    let plan = w.plan(&pkg, Some(&w.root())).unwrap();
    let inbox = plan.pack_inbox.as_ref().expect("the plan names the packs");
    assert_eq!(inbox.user_root, w.root());
    assert_eq!(inbox.dir, w.inbox());
    let files: Vec<_> = inbox
        .files
        .iter()
        .map(|f| (f.name.as_str(), f.uid.as_str(), f.tombstone.as_str()))
        .collect();
    assert_eq!(files, [(PACK, UID, TOMBSTONE)]);
    assert_eq!(inbox.files[0].sha256, sha(&astrapack(UID)));
    assert!(!w.root().exists(), "planning writes nothing");
    assert_eq!(plan.profile_files, 2, "the pack is not the profile's");

    let ledger = install(&pkg, &plan, &w.target(), ACCEPT, &Ctx::silent()).unwrap();
    assert_eq!(fs::read(w.inbox().join(PACK)).unwrap(), astrapack(UID));
    assert_eq!(w.holds(), 1);
    assert!(ledger.files.iter().all(|f| !f.rel.contains("astrapack")));
    let record = ledger.pack_inbox.as_ref().unwrap();
    assert_eq!(record.user_root, w.root());
    assert_eq!(
        (
            record.files[0].name.as_str(),
            record.files[0].tombstone.as_str()
        ),
        (PACK, TOMBSTONE)
    );

    w.engine_imports();
    prepare_launch(&w.profile(), &w.target(), &vars(), None).expect("Play never asks for a pack");
}

/// No root given: refused at plan with its own code, never a guessed path — by every planning
/// entry point. A package that places no pack plans the same with a root or without one.
#[test]
fn without_a_root_the_plan_is_refused() {
    let w = World::new();
    let pkg = climber("1.0.0");
    for e in [
        w.plan(&pkg, None).unwrap_err(),
        plan_install(&pkg, &w.target(), &w.profile()).unwrap_err(),
        plan_install_in(
            &pkg,
            &w.target(),
            &w.profile(),
            &EngineSet::empty(),
            Provenance::FirstParty,
            None,
        )
        .unwrap_err(),
    ] {
        assert_eq!(
            (e.code, e.param("reason"), e.param("files")),
            (
                codes::PACK_INBOX_UNAVAILABLE,
                Some("no-user-root"),
                Some(PACK)
            )
        );
    }
    assert!(!w.profile().exists() && !w.root().exists());

    let plain_runner = runner();
    let without = plan_install(&plain_runner, &w.target(), &w.profile()).unwrap();
    let with = w.plan(&plain_runner, Some(&w.root())).unwrap();
    assert_eq!(with, without);
    assert!(
        serde_json::to_value(&with)
            .unwrap()
            .get("pack_inbox")
            .is_none(),
        "absent from the JSON"
    );

    // A root that is a file is no place for them either.
    write(&w.root(), b"not a folder");
    let e = w.plan(&pkg, Some(&w.root())).unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::PACK_INBOX_UNAVAILABLE, Some("not-a-directory"))
    );
}

/// Uninstall writes the design's tombstone, `<uid tail>.remove`, into the same inbox folder —
/// named after the uid the archive's `animpack.toml` says, never the archive's name — and empty.
/// The hold goes. A pack the companion had not imported yet is removed first; one that changed
/// since is not ours to remove.
#[test]
fn uninstall_writes_the_tombstone_into_the_inbox() {
    let w = World::new();
    w.install(&climber("1.0.0"));
    w.engine_imports();
    let r = w.uninstall();
    let p = r.pack_inbox.expect("the report says what it did there");
    assert_eq!(p.tombstones, [TOMBSTONE]);
    assert!(p.removed.is_empty() && p.held.is_empty() && p.skipped.is_none());
    assert_eq!(
        w.inbox_names(),
        [TOMBSTONE],
        "no hold left, no `peak-climb.remove`"
    );
    assert_eq!(fs::read(w.inbox().join(TOMBSTONE)).unwrap(), b"");
    assert_eq!(installed(&w.profile()).unwrap(), None);

    // Not imported yet: removed, and the tombstone written all the same (an older version of it
    // may be installed).
    let w = World::new();
    w.install(&climber("1.0.0"));
    let p = w.uninstall().pack_inbox.unwrap();
    assert_eq!(
        (p.removed, p.tombstones),
        (vec![PACK.to_owned()], vec![TOMBSTONE.to_owned()])
    );
    assert_eq!(w.inbox_names(), [TOMBSTONE]);

    // Changed since it was placed: someone else's now, left.
    let w = World::new();
    w.install(&climber("1.0.0"));
    fs::write(w.inbox().join(PACK), b"another pack").unwrap();
    let p = w.uninstall().pack_inbox.unwrap();
    assert!(p.removed.is_empty());
    assert_eq!(w.inbox_names(), [TOMBSTONE, PACK]);

    // The companion's folder is gone, and the packs with it: nothing to write, and said so.
    let w = World::new();
    w.install(&climber("1.0.0"));
    fs::remove_dir_all(w.root()).unwrap();
    let p = w.uninstall().pack_inbox.unwrap();
    assert_eq!(p.skipped.as_deref(), Some("root-missing"));
    assert!(!w.root().exists(), "never made again");
}

/// A runner installed into two games delivers into ONE inbox folder: uninstalling it from one
/// game leaves the pack the other still uses (its hold names it), and the last uninstall writes
/// the tombstone. A hold whose profile was removed by hand holds nothing.
#[test]
fn a_pack_another_install_holds_gets_no_tombstone() {
    let a = World::new();
    let b = World {
        game: Tmp::new("game-b"),
        profiles: Tmp::new("profiles-b"),
        companion: Tmp::new("unused"),
    };
    unity_game(b.game.path(), GAME);
    let plan_b = |pkg: &GiPackage| {
        plan_install_with_user_pack_root(
            pkg,
            &b.target(),
            &b.profile(),
            &EngineSet::empty(),
            Provenance::FirstParty,
            None,
            Some(&a.root()),
        )
        .unwrap()
    };
    let pkg = climber("1.0.0");
    a.install(&pkg);
    install(&pkg, &plan_b(&pkg), &b.target(), ACCEPT, &Ctx::silent()).unwrap();
    assert_eq!(a.holds(), 2);
    a.engine_imports();

    let p = a.uninstall().pack_inbox.unwrap();
    assert_eq!(
        (p.tombstones.len(), p.held),
        (0, vec![TOMBSTONE.to_owned()])
    );
    assert_eq!(a.holds(), 1);
    assert!(
        !a.inbox().join(TOMBSTONE).exists(),
        "the other game still has it"
    );
    let p = uninstall(&b.profile(), &b.target())
        .unwrap()
        .pack_inbox
        .unwrap();
    assert_eq!(p.tombstones, [TOMBSTONE]);
    assert_eq!(a.inbox_names(), [TOMBSTONE]);

    // A hold of a profile deleted by hand (no ledger behind it) holds nothing.
    fs::remove_file(a.inbox().join(TOMBSTONE)).unwrap();
    a.install(&pkg);
    install(&pkg, &plan_b(&pkg), &b.target(), ACCEPT, &Ctx::silent()).unwrap();
    fs::remove_dir_all(b.profile()).unwrap();
    let p = a.uninstall().pack_inbox.unwrap();
    assert_eq!(p.tombstones, [TOMBSTONE]);
}

/// An update that hands over the same pack again writes no tombstone (the companion replaces the
/// pack by its version; a tombstone would take it off whoever wears it), and one that no longer
/// ships it writes one. An install cut off after the pack landed rolls it back: removed, its
/// tombstone written, its hold gone.
#[test]
fn an_update_writes_a_tombstone_only_for_a_pack_it_drops() {
    let w = World::new();
    w.install(&climber("1.0.0"));
    w.engine_imports();
    let l = w.install(&climber("1.1.0"));
    assert_eq!(l.integration.version, "1.1.0");
    assert_eq!(
        w.inbox_names()
            .iter()
            .filter(|n| !n.ends_with(".holds"))
            .collect::<Vec<_>>(),
        [PACK]
    );
    assert_eq!(w.holds(), 1);

    w.engine_imports();
    let l = w.install(&open(&with_packs("", "1.2.0", &[])).unwrap());
    assert!(l.pack_inbox.is_none());
    assert_eq!(
        w.inbox_names(),
        [TOMBSTONE],
        "the dropped pack's tombstone; no hold"
    );

    // Cancelled once the pack is placed: rolled back.
    let w = World::new();
    let pkg = climber("1.0.0");
    let plan = w.plan(&pkg, Some(&w.root())).unwrap();
    let placed = Cell::new(false);
    let cancel = || placed.get();
    let progress = |p: astra_game_launcher::Progress| {
        if p.stage == "pack-inbox" {
            placed.set(true);
        }
    };
    let ctx = Ctx {
        cancel: &cancel,
        progress: &progress,
    };
    let e = install(&pkg, &plan, &w.target(), ACCEPT, &ctx).unwrap_err();
    assert_eq!(e.code, codes::CANCELLED);
    assert_eq!(installed(&w.profile()).unwrap(), None);
    assert_eq!(w.inbox_names(), [TOMBSTONE]);
}

/// The path is jailed: a destination that leaves the inbox, a folder below it, a file that is no
/// pack, a root that overlaps the game, and a link on the way below the root — at plan, at
/// install (a link made after the plan), and at uninstall (never written through).
#[test]
fn the_inbox_is_jailed() {
    for (to, code, reason) in [
        (
            "${astra_pack_inbox}/../x.astrapack",
            codes::PATH_ESCAPES,
            None,
        ),
        (
            "${astra_pack_inbox}/a/../../x.astrapack",
            codes::PATH_ESCAPES,
            None,
        ),
        ("${astra_pack_inbox}x.astrapack", codes::PATH_ESCAPES, None),
        (
            "${astra_pack_inbox}/C:x.astrapack",
            codes::PATH_ESCAPES,
            None,
        ),
        (
            "${astra_pack_inbox}/NUL.astrapack",
            codes::PATH_ESCAPES,
            None,
        ),
        (
            "${astra_pack_inbox}/sub/x.astrapack",
            codes::PACK_INBOX_INVALID,
            Some("in-a-folder"),
        ),
        (
            "${astra_pack_inbox}/sub/",
            codes::PACK_INBOX_INVALID,
            Some("in-a-folder"),
        ),
        (
            "${astra_pack_inbox}/x.zip",
            codes::PACK_INBOX_INVALID,
            Some("not-a-pack"),
        ),
        (
            "${astra_pack_inbox}/x.remove",
            codes::PACK_INBOX_INVALID,
            Some("not-a-pack"),
        ),
        (
            "${astra_pack_inbox}/.x.astrapack",
            codes::PACK_INBOX_INVALID,
            Some("not-a-pack"),
        ),
    ] {
        let rules = format!("\n[[files]]\nfrom = \"packs/{PACK}\"\nto = \"{to}\"\n");
        let e = Manifest::parse(&runner_manifest(&rules)).unwrap_err();
        assert_eq!((e.code, e.param("reason")), (code, reason), "{to}");
    }
    // A `**` expansion lands each name straight in the folder, and only packs.
    for (entry, reason) in [
        ("packs/sub/x.astrapack", "in-a-folder"),
        ("packs/readme.txt", "not-a-pack"),
    ] {
        let e = refusal(&with_packs(INTO_INBOX, "1.0.0", &[(entry, astrapack(UID))]));
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::PACK_INBOX_INVALID, Some(reason)),
            "{entry}"
        );
    }

    let w = World::new();
    let pkg = climber("1.0.0");
    let e = w.plan(&pkg, Some(&w.game.join("pack"))).unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::PATH_ESCAPES, Some("pack-inbox-overlaps-game"))
    );
    // A folder where the pack goes.
    fs::create_dir_all(w.inbox().join(PACK)).unwrap();
    assert_eq!(
        w.plan(&pkg, Some(&w.root())).unwrap_err().code,
        codes::PATH_ESCAPES
    );

    // A ledger is a file on disk: a record this launcher never writes (a tombstone that climbs
    // out, a share for a root) is never acted on, and never keeps the uninstall from finishing.
    for (field, value) in [
        ("tombstone", serde_json::json!("../../../escaped.remove")),
        ("user_root", serde_json::json!(r"\\server\share\pack")),
    ] {
        let w = World::new();
        w.install(&pkg);
        let path = w.profile().join("astra-launcher-ledger.json");
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        match field {
            "tombstone" => v["pack_inbox"]["files"][0]["tombstone"] = value,
            _ => v["pack_inbox"]["user_root"] = value,
        }
        fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let p = w.uninstall().pack_inbox.unwrap();
        assert_eq!(p.skipped.as_deref(), Some("path-escapes"), "{field}");
        assert!(p.tombstones.is_empty(), "{field}");
        assert!(
            !w.root().join("escaped.remove").exists()
                && !w.companion.join("escaped.remove").exists()
        );
        assert_eq!(
            installed(&w.profile()).unwrap(),
            None,
            "{field}: the uninstall finished"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = Tmp::new("outside");
        let empty = |p: &Path| fs::read_dir(p).unwrap().next().is_none();

        // At plan: a link on the way below the root.
        let w = World::new();
        fs::create_dir_all(w.root()).unwrap();
        symlink(outside.path(), w.root().join("inbox")).unwrap();
        let e = w.plan(&pkg, Some(&w.root())).unwrap_err();
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::PATH_ESCAPES, Some("link"))
        );

        // At install: a link made after the plan.
        let w = World::new();
        let plan = w.plan(&pkg, Some(&w.root())).unwrap();
        fs::create_dir_all(w.root().join("inbox")).unwrap();
        symlink(outside.path(), w.root().join("inbox/packs")).unwrap();
        let e = install(&pkg, &plan, &w.target(), ACCEPT, &Ctx::silent()).unwrap_err();
        assert_eq!(e.code, codes::PATH_ESCAPES);
        assert!(empty(outside.path()), "nothing written outside");
        assert_eq!(installed(&w.profile()).unwrap(), None);

        // At uninstall: the inbox replaced by a link is never written through.
        let w = World::new();
        w.install(&pkg);
        fs::remove_dir_all(w.root().join("inbox/packs")).unwrap();
        symlink(outside.path(), w.root().join("inbox/packs")).unwrap();
        let p = w.uninstall().pack_inbox.unwrap();
        assert_eq!(p.skipped.as_deref(), Some("path-escapes"));
        assert!(empty(outside.path()), "no tombstone through the link");
        assert_eq!(
            installed(&w.profile()).unwrap(),
            None,
            "the uninstall still finished"
        );
    }
}

/// The root a caller names is judged by its text as Windows reads it on every machine before
/// anything touches it: a share, a device, a verbatim namespace, a drive-relative path, `..`.
/// And a uid whose tombstone Windows would open as a device is refused when the package opens.
#[test]
fn windows_path_forms_are_refused_everywhere() {
    let w = World::new();
    let pkg = climber("1.0.0");
    for (root, reason) in [
        (r"\\server\share\pack", "unc"),
        ("//server/share/pack", "unc"),
        (r"\\?\UNC\server\share\pack", "verbatim-unc"),
        (r"\\.\pipe\pack", "device"),
        (r"\\?\GLOBALROOT\Device\HarddiskVolume1\pack", "verbatim"),
        ("C:pack", "not-absolute"),
        ("relative/pack", "not-absolute"),
    ] {
        let e = w.plan(&pkg, Some(Path::new(root))).unwrap_err();
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::PATH_ESCAPES, Some(reason)),
            "{root}"
        );
    }
    let parent = w.companion.join("x/../pack");
    let e = w.plan(&pkg, Some(&parent)).unwrap_err();
    assert_eq!(
        (e.code, e.param("reason")),
        (codes::PATH_ESCAPES, Some("parent"))
    );
    if !cfg!(windows) {
        // A drive letter is a relative name here, never a drive.
        let e = w
            .plan(&pkg, Some(Path::new(r"C:\Users\me\pack")))
            .unwrap_err();
        assert_eq!(e.param("reason"), Some("not-absolute"));
    }
    for tail in ["con", "nul", "aux", "com1", "lpt1"] {
        let uid = format!("test.unity/{tail}");
        let e = refusal(&with_packs(
            INTO_INBOX,
            "1.0.0",
            &[(PACK_ENTRY, astrapack(&uid))],
        ));
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::PACK_INBOX_INVALID, Some("uid")),
            "{tail}"
        );
    }
}

/// The archive is read for its uid when the package opens, as the companion's importer reads it:
/// `animpack.toml` at its root or in its one top-level folder, `[animpack] uid` this
/// integration's, a tail the companion reads; the integration id itself one it reads as a folder.
#[test]
fn the_pack_says_a_uid_of_this_integration() {
    let one = |pack: Vec<u8>| with_packs(INTO_INBOX, "1.0.0", &[(PACK_ENTRY, pack)]);
    let reason = |zip: Vec<u8>| {
        let e = refusal(&zip);
        assert_eq!(e.code, codes::PACK_INBOX_INVALID, "{e}");
        e.param("reason").unwrap_or_default().to_owned()
    };
    assert!(
        open(&one(astrapack_at("PEAK climbing/", UID))).is_ok(),
        "one top-level folder"
    );
    assert_eq!(reason(one(astrapack("other.game/climb"))), "uid");
    assert_eq!(reason(one(astrapack("test.unity/Climb"))), "uid");
    assert_eq!(reason(one(astrapack("test.unity/.climb"))), "uid");
    assert_eq!(reason(one(astrapack("test.unity"))), "uid");
    assert_eq!(reason(one(astrapack(""))), "uid");
    assert_eq!(reason(one(b"not a zip".to_vec())), "not-a-zip");
    assert_eq!(
        reason(one(zip(&[E::File("clips/climb.vrma", b"x")]))),
        "no-animpack-toml"
    );
    let two_folders = zip(&[
        E::File(
            "a/animpack.toml",
            format!("[animpack]\nuid = \"{UID}\"\n").as_bytes(),
        ),
        E::File("b/x.txt", b"x"),
    ]);
    assert_eq!(reason(one(two_folders)), "no-animpack-toml");
    assert_eq!(
        reason(one(zip(&[E::File("animpack.toml", b"[animpack\nuid=")]))),
        "unreadable"
    );
    let twice = with_packs(
        INTO_INBOX,
        "1.0.0",
        &[
            ("packs/a.astrapack", astrapack(UID)),
            ("packs/b.astrapack", astrapack(UID)),
        ],
    );
    assert_eq!(reason(twice), "uid-twice");

    // An id the companion does not read as an inbox folder.
    for id in ["Test.Unity", ".hidden", "a."] {
        let m = runner_manifest(INTO_INBOX).replacen(
            "id = \"test.unity\"",
            &format!("id = \"{id}\""),
            1,
        );
        let e = Manifest::parse(&m).unwrap_err();
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::PACK_INBOX_INVALID, Some("integration-id")),
            "{id}"
        );
    }
    // A destination only: as a caller's variable, refused.
    let m = runner_manifest("").replacen("\"--doorstop-enabled\"", "\"${astra_pack_inbox}\"", 1);
    let e = Manifest::parse(&m).unwrap_err();
    assert_eq!(
        (e.code, e.param("name")),
        (codes::UNKNOWN_PLACEHOLDER, Some("astra_pack_inbox"))
    );
}

/// A pack is held to what the release around it is (`zip_consistency::check_nested`) before its
/// uid is believed: one archive to every reader — a field that renames an entry for some readers,
/// a byte before its first entry — and no entry no reader may read (encrypted: here over the
/// stored-dd trick its check passes over; another method), each refused with `pack` naming it.
/// The same pack without the one thing opens.
#[test]
fn a_pack_is_held_to_what_the_release_is() {
    use common::rawzip::{Raw, archive, descriptor};
    let toml = format!("[animpack]\nuid = \"{UID}\"\n");
    let pack = |clip: Raw| archive(&[Raw::stored("animpack.toml", toml.as_bytes()), clip]);
    let one = |p: Vec<u8>| with_packs(INTO_INBOX, "1.0.0", &[(PACK_ENTRY, p)]);
    let clip = || Raw::stored("clips/climb.vrma", b"glTF fake vrma");
    open(&one(pack(clip()))).expect("the near miss: the same pack, nothing wrong with it");

    let upath = {
        let mut body = vec![1u8];
        body.extend_from_slice(&crc32fast::hash(b"clips/climb.vrma").to_le_bytes());
        body.extend_from_slice(b"clips/run.exe");
        let mut f = 0x7075u16.to_le_bytes().to_vec();
        f.extend_from_slice(&(body.len() as u16).to_le_bytes());
        f.extend_from_slice(&body);
        f
    };
    let mut hidden = b"x".to_vec();
    hidden.extend_from_slice(&descriptor(crc32fast::hash(b"x"), 1, 1));
    hidden.extend_from_slice(b"PK\x03\x04 the next header, for a reader of local headers");
    for (why, clip, code, reason) in [
        (
            "a Unicode Path field",
            Raw {
                local_extra: upath.clone(),
                central_extra: upath.clone(),
                ..clip()
            },
            codes::ZIP_INCONSISTENT,
            "unicode-path",
        ),
        (
            "a byte before the clip",
            Raw {
                before: vec![0],
                ..clip()
            },
            codes::ZIP_INCONSISTENT,
            "gap",
        ),
        (
            "an encrypted entry hiding a descriptor",
            Raw {
                flags: (1 << 3) | 1,
                ..Raw::streamed("clips/climb.vrma", &hidden)
            },
            codes::ZIP_UNSAFE_ENTRY,
            "encrypted",
        ),
        (
            "an entry of another method",
            Raw {
                method: 12,
                ..clip()
            },
            codes::ZIP_UNSAFE_ENTRY,
            "compression",
        ),
    ] {
        let e = refusal(&one(pack(clip)));
        assert_eq!(
            (e.code, e.param("reason"), e.param("pack")),
            (code, Some(reason), Some(PACK_ENTRY)),
            "{why}: {e}"
        );
    }
}

/// Every pack of a release is read within the ONE budget of the limits it is opened with: two
/// packs that each fit, together do not, and the second is refused `too-large` where a fresh
/// budget per pack let N packs of a bomb cost N times what one may.
#[test]
fn every_pack_of_a_release_shares_its_one_budget() {
    let zeros = vec![0u8; 150 << 10];
    let pack = |uid: &str| {
        zip(&[
            E::File(
                "animpack.toml",
                format!("[animpack]\nuid = \"{uid}\"\n").as_bytes(),
            ),
            E::File("clips/zeros.bin", &zeros),
        ])
    };
    let limits = astra_game_launcher::Limits {
        max_uncompressed_bytes: 256 << 10,
        ..Default::default()
    };
    let open_with = |z: &[u8]| GiPackage::open_with_limits(z, &sha(z), &limits);
    open_with(&with_packs(
        INTO_INBOX,
        "1.0.0",
        &[("packs/a.astrapack", pack("test.unity/a"))],
    ))
    .expect("one pack fits");
    let two = with_packs(
        INTO_INBOX,
        "1.0.0",
        &[
            ("packs/a.astrapack", pack("test.unity/a")),
            ("packs/b.astrapack", pack("test.unity/b")),
        ],
    );
    let e = open_with(&two).unwrap_err();
    assert_eq!(
        (e.code, e.param("uncompressed"), e.param("pack")),
        (codes::TOO_LARGE, Some("true"), Some("packs/b.astrapack")),
        "{e}"
    );
    open(&two).expect("both fit the default limits");
}

/// Launcher 0.3.1 (Astra 0.2.7) and 0.4.1 refuse the placeholder as they parse the manifest, and
/// the ledger of an install that handed the companion a pack is schema 5, which both refuse as
/// too new — never an uninstall that would leave the pack in the companion.
#[test]
fn launchers_before_it_refuse_the_manifest_and_the_ledger() {
    let z = with_packs(INTO_INBOX, "1.0.0", &[(PACK_ENTRY, astrapack(UID))]);
    let e = launcher_031::GiPackage::open(&z, &sha(&z)).unwrap_err();
    assert_eq!(
        (e.code, e.param("name")),
        (
            launcher_031::codes::UNKNOWN_PLACEHOLDER,
            Some("astra_pack_inbox")
        )
    );
    let e = launcher_legacy::GiPackage::open(&z, &sha(&z)).unwrap_err();
    assert_eq!(e.code, launcher_legacy::codes::UNKNOWN_PLACEHOLDER);

    let w = World::new();
    w.install(&climber("1.0.0"));
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(w.profile().join("astra-launcher-ledger.json")).unwrap())
            .unwrap();
    assert_eq!(v["schema"], 5);
    assert_eq!(astra_game_launcher::LEDGER_SCHEMA, 5);
    let e = launcher_031::installed(&w.profile()).unwrap_err();
    assert_eq!(
        (e.code, e.param("schema")),
        (launcher_031::codes::SCHEMA_TOO_NEW, Some("5"))
    );
    let e = launcher_legacy::installed(&w.profile()).unwrap_err();
    assert_eq!(
        (e.code, e.param("schema")),
        (launcher_legacy::codes::SCHEMA_TOO_NEW, Some("5"))
    );
    // A plain install is still the lowest schema.
    let w = World::new();
    w.install(&runner());
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(w.profile().join("astra-launcher-ledger.json")).unwrap())
            .unwrap();
    assert_eq!(v["schema"], 1);
    assert!(v.get("pack_inbox").is_none());
}

/// The command line: `plan --user-pack-root <dir>` names the root; without it the plan is refused
/// with the same code.
#[test]
fn the_cli_takes_the_root_as_a_flag() {
    let w = World::new();
    let z = with_packs(INTO_INBOX, "1.0.0", &[(PACK_ENTRY, astrapack(UID))]);
    let zip_path = w.companion.join("astra-gi.zip");
    fs::write(&zip_path, &z).unwrap();
    let run = |extra: &[&std::ffi::OsStr]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_astra-game-launcher"))
            .arg("plan")
            .arg(&zip_path)
            .arg("--sha256")
            .arg(sha(&z))
            .arg("--exe")
            .arg(format!("{GAME}.exe"))
            .args(["--platform", "windows", "--json"])
            .arg("--game")
            .arg(w.game.path())
            .arg("--profile")
            .arg(w.profile())
            .args(extra)
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        (out.status.success(), v)
    };
    let (ok, v) = run(&["--user-pack-root".as_ref(), w.root().as_os_str()]);
    assert!(ok, "{v}");
    assert_eq!(v["pack_inbox"]["dir"], w.inbox().to_str().unwrap());
    assert_eq!(v["pack_inbox"]["files"][0]["tombstone"], TOMBSTONE);
    let (ok, v) = run(&[]);
    assert!(!ok);
    assert_eq!(v["code"], codes::PACK_INBOX_UNAVAILABLE, "{v}");
}
