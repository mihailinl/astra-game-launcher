// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `${astra_pack_inbox}`: a game integration's own animation packs, handed to the desktop
//! companion (astra-companion `docs/animation-packs.md` §2, §5 (b), §9 P6; the engine's importer
//! is its `config/animpack/ops/inbox.rs`).
//!
//! A `[[files]]` entry whose destination is `${astra_pack_inbox}/` or
//! `${astra_pack_inbox}/<name>.astrapack` places `.astrapack` archives in
//! `<user root>/inbox/packs/<integration id>/`. `<user root>` is the companion's USER root, the
//! `ASTRA_USER_PACK` Astra pairs with the engine (the daemon's `CompanionLocation`), and only the
//! caller can name it ([`crate::plan_install_with_user_pack_root`], the CLI's
//! `--user-pack-root`): a plan of a package that places one without it is
//! `pack-inbox-unavailable{reason=no-user-root}`, never a guessed path.
//!
//! - **What may go there**: `.astrapack` files only (any letter case of the extension, not
//!   dot-led), straight in the folder (never below it: the engine reads only the folder's own
//!   files), each read from an entry of the release that is itself a content-typed `.astrapack`
//!   (so the content check sees what the companion imports: `reason=not-content-typed`
//!   otherwise), each one a zip whose `animpack.toml` (at its root, or in its one top-level
//!   folder, as the engine looks; a case variant there is `animpack-toml-case`) says
//!   `[animpack] uid = "<integration id>/<tail>"` — the one claim the
//!   engine holds an inbox archive to that names it, and the one the tombstone needs
//!   (`pack-inbox-invalid{file, reason}` otherwise, when the package is opened). Before that, the
//!   archive is held to what the release around it is (`zip-inconsistent`, `zip-unsafe-entry`,
//!   with `pack` naming it), within the release's one budget (`too-large`). The integration
//!   id itself must be one the engine reads as an inbox folder: `[a-z0-9._-]{1,64}`, not dot-led
//!   (`pack-inbox-invalid{key=integration.id}` at parse). The archive's other claims (`owner`,
//!   `games`, `version`, no `patches`) are the engine's to judge at import: one that breaks them
//!   is set aside in the inbox's `.refused/` (debt row 41).
//! - **The jail**: the root is an absolute local path (no share, device or verbatim namespace,
//!   judged by its text as Windows reads it on every machine, and no `..`); below it, every
//!   folder is created one by one, a link on the way is refused, and the resolved folder must
//!   still be below the root ([`crate::paths::prepare_target`]); a root that overlaps the game's
//!   folder is refused. Every write is a temporary dot-led file plus a rename, so the engine (which
//!   leaves dot-led names alone) never reads half a pack.
//! - **Delivered, not kept**: the engine imports a pack and deletes its file, so Play never asks
//!   for one. The ledger records them apart from the profile's files ([`LedgerPackInbox`]); an
//!   uninstall removes one still waiting there with its digest, then writes the design's
//!   TOMBSTONE into the same folder, `<tail>.remove`, which makes the engine trash the pack
//!   `<integration id>/<tail>` if it is that integration's. `<tail>` is the uid's, read from the
//!   archive — never the file's name: the design's own example ships `peak-climb.astrapack` with
//!   uid `astra.peak/climb` and tombstone `climb.remove`.
//! - **One folder per integration, whichever game**: a runner installed into two games delivers
//!   into one folder. Every profile that delivered a pack leaves a HOLD beside it
//!   (`.astra-launcher-<key>.holds`, dot-led, so the engine never reads it) naming the tombstones
//!   its own uninstall would write; an uninstall removes a waiting pack and writes a tombstone
//!   only when no live hold of another profile names it (one whose profile still holds a ledger).
//!   An update that delivers the same pack (the same uid) again writes none for it (the engine
//!   replaces the pack by its version, and a tombstone would take it off the characters wearing
//!   it), unless it stops before its own ledger records the pack: then it writes it after all.
//!
//! Launcher 0.3.1 (Astra 0.2.7) refuses the placeholder when it parses the manifest
//! (`unknown-placeholder`), so a release that uses it needs `min_astra` 0.2.8; `gi-probe compat`
//! says so (`needs-newer-astra{min_astra}`). A ledger that records one is schema 5, which 0.3.1
//! and 0.4.1 refuse (`schema-too-new`) rather than uninstall without writing the tombstone.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Cursor, ErrorKind, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use crate::ctx::Ctx;
use crate::digest::sha256_hex;
use crate::error::{LauncherError, Result, codes};
use crate::ledger::Ledger;
use crate::package::{GiPackage, PackFile};
use crate::paths::{
    FileState, JailPath, LEDGER_FILE, absolute, canonical_dir, check_component,
    check_regular_or_absent, file_state, plain, prepare_target, require_local, resolve_lenient,
    windows_text_verdict, write_atomic,
};

/// The placeholder's name.
pub(crate) const VARIABLE: &str = "astra_pack_inbox";
/// The destination root as a manifest writes it.
pub(crate) const DESTINATION: &str = "${astra_pack_inbox}";
/// The folders below the user root: `<user root>/inbox/packs/<integration id>/`.
const INBOX_DIRS: [&str; 2] = ["inbox", "packs"];
/// The only files the engine imports from its inbox (`archive::PACK_ARCHIVE_EXT`).
const ARCHIVE_EXT: &str = ".astrapack";
/// A tombstone's extension (the engine's `TOMBSTONE_EXT`).
const TOMBSTONE_EXT: &str = ".remove";
/// The pack's manifest inside its archive (the engine's `PACK_FILE`).
const PACK_FILE: &str = "animpack.toml";
/// The most of `animpack.toml` read for its uid.
const MAX_PACK_FILE: u64 = 1024 * 1024;
/// A hold's name: `.astra-launcher-<16 hex digits>.holds`.
const HOLD_PREFIX: &str = ".astra-launcher-";
const HOLD_EXT: &str = ".holds";
/// The most of another profile's hold read.
const MAX_HOLD: u64 = 64 * 1024;

/// `${astra_pack_inbox}`'s packs, as an install would place them: the plan's
/// [`crate::InstallPlan::pack_inbox`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackInboxPlan {
    /// The companion's user root the caller named (`ASTRA_USER_PACK`), absolute, as given.
    pub user_root: PathBuf,
    /// `<user root>/inbox/packs/<integration id>/`: where the packs go.
    pub dir: PathBuf,
    /// In the manifest's order.
    pub files: Vec<PackInboxFile>,
}

/// One pack an install hands the companion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackInboxFile {
    /// Its file name in the inbox folder.
    pub name: String,
    pub sha256: String,
    pub size: u64,
    /// `[animpack] uid` as its `animpack.toml` says it: `<integration id>/<tail>`.
    pub uid: String,
    /// What an uninstall writes beside it: `<tail>.remove`.
    pub tombstone: String,
}

/// The packs an install handed the companion: [`crate::Ledger::pack_inbox`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerPackInbox {
    /// The user root they went below, as the plan gave it.
    pub user_root: PathBuf,
    pub files: Vec<LedgerPackFile>,
}

/// One pack an install handed the companion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerPackFile {
    /// Its file name in `<user root>/inbox/packs/<integration id>/`.
    pub name: String,
    pub sha256: String,
    /// `<tail>.remove`, written there at uninstall.
    pub tombstone: String,
}

/// What an uninstall did in the companion's inbox: [`crate::UninstallReport::pack_inbox`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackInboxReport {
    /// Packs still waiting in the inbox (the engine had not imported them), with our digest:
    /// removed.
    #[serde(default)]
    pub removed: Vec<String>,
    /// Tombstones written: the engine trashes each pack.
    #[serde(default)]
    pub tombstones: Vec<String>,
    /// Tombstones not written because another profile's install still holds the pack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<String>,
    /// Nothing was done, and why: `root-missing` (the user root is gone: so is the companion
    /// data the packs went into), `path-escapes` (a link or a file on the way into the inbox:
    /// never written through).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// A pack of the package: [`GiPackage`]'s, read when it is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InboxPack {
    /// `rel` is the one name in the inbox folder.
    pub(crate) file: PackFile,
    pub(crate) uid: String,
    pub(crate) tombstone: String,
}

impl InboxPack {
    pub(crate) fn name(&self) -> &str {
        self.file.rel.last().map(String::as_str).unwrap_or_default()
    }
}

fn invalid(reason: &str) -> LauncherError {
    LauncherError::new(codes::PACK_INBOX_INVALID).with("reason", reason)
}

fn escapes(path: &Path, reason: &str) -> LauncherError {
    LauncherError::new(codes::PATH_ESCAPES)
        .with("path", path.display().to_string())
        .with("reason", reason)
}

/// The engine's `valid_integration_id`: `[a-z0-9._-]{1,64}`, not dot-led — an inbox folder name
/// and a uid's tail alike.
pub(crate) fn engine_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// The integration id as the inbox folder's name: one the engine reads, and a name Windows keeps
/// as written (no trailing dot, no device name).
fn folder_id_ok(id: &str) -> bool {
    engine_id(id) && check_component(id)
}

/// A name the engine imports from its inbox: `<stem>.astrapack`, not dot-led.
fn is_pack_name(name: &str) -> bool {
    !name.starts_with('.')
        && check_component(name)
        && name.len() > ARCHIVE_EXT.len()
        && name.to_ascii_lowercase().ends_with(ARCHIVE_EXT)
}

/// A tombstone name the engine reads: `<tail>.remove`, `<tail>` a uid tail.
fn is_tombstone_name(name: &str) -> bool {
    name.strip_suffix(TOMBSTONE_EXT).is_some_and(engine_id) && check_component(name)
}

/// [`Manifest::parse`](crate::Manifest::parse)'s check of one `[[files]]` entry whose
/// destination is `${astra_pack_inbox}` (`key` = `files[n].to`): the integration id is an inbox
/// folder the engine reads, the destination is the folder itself (`${astra_pack_inbox}/`) or one
/// `.astrapack` straight in it.
pub(crate) fn check_rule(id: &str, to: &JailPath, key: &str) -> Result<()> {
    if !folder_id_ok(id) {
        return Err(invalid("integration-id")
            .with("key", "integration.id")
            .with(
                "detail",
                "the companion's inbox reads [a-z0-9._-] ids, not dot-led",
            ));
    }
    if to.rel.len() > 1 || (to.is_dir && !to.rel.is_empty()) {
        return Err(invalid("in-a-folder").with("key", key));
    }
    if let Some(name) = to.rel.first()
        && !is_pack_name(name)
    {
        return Err(invalid("not-a-pack")
            .with("key", key)
            .with("file", name.clone()));
    }
    Ok(())
}

/// [`GiPackage::open`]'s check of one file a `${astra_pack_inbox}` entry places (a `dir/**`
/// expansion included): one `.astrapack` straight in the folder.
pub(crate) fn check_placed(f: &PackFile) -> Result<()> {
    match f.rel.as_slice() {
        [name] if is_pack_name(name) => Ok(()),
        [name] => Err(invalid("not-a-pack")
            .with("file", f.entry.clone())
            .with("name", name.clone())),
        _ => Err(invalid("in-a-folder").with("file", f.entry.clone())),
    }
}

/// The pack's uid and its tombstone, from the archive's own bytes (`entry` names it in the
/// release zip, a content-typed `.astrapack`): its `animpack.toml`, at the archive's root or in
/// its one top-level folder, as the engine's importer finds it (in no other letter case), says
/// `[animpack] uid = "<id>/<tail>"` with a tail the engine reads. The archive is first held to what the release zip around it is
/// ([`crate::zip_consistency::check_nested`]: one archive to every reader, no entry no reader may
/// read), within the release's one `budget`.
pub(crate) fn read_uid(
    entry: &str,
    bytes: &[u8],
    id: &str,
    budget: &mut crate::zip_consistency::Budget,
) -> Result<(String, String)> {
    let bad = |reason: &str| invalid(reason).with("file", entry);
    // What the companion imports is a file the release's content check sees: the release's own
    // entry a content-typed `.astrapack` (that name, a zip's magic at offset 0), never bytes
    // renamed into one by the destination or behind a prefix.
    if crate::content::extension(entry) != ARCHIVE_EXT
        || !crate::content::is_content_typed(entry, bytes)
    {
        return Err(bad("not-content-typed"));
    }
    let mut ar = ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| bad("not-a-zip").with("detail", e.to_string()))?;
    crate::zip_consistency::check_nested(&mut ar, bytes, budget)
        .map_err(|e| e.with("pack", entry))?;

    let mut names: Vec<(usize, String, u64)> = Vec::new();
    for i in 0..ar.len() {
        let f = ar
            .by_index_raw(i)
            .map_err(|e| bad("not-a-zip").with("detail", e.to_string()))?;
        let name = f
            .name()
            .map_err(|e| bad("not-a-zip").with("detail", e.to_string()))?
            .replace('\\', "/");
        names.push((i, name, f.size()));
    }
    // `animpack.toml` at `want`, as every engine finds it: the engine asks the file system, which
    // on Windows and macOS matches any letter case. So a case variant there is refused, never
    // passed over: an engine there would read it where this reads another (or none).
    let pack_file = |want: &str| -> Result<Option<&(usize, String, u64)>> {
        let like: Vec<&(usize, String, u64)> = names
            .iter()
            .filter(|(_, n, _)| n.eq_ignore_ascii_case(want))
            .collect();
        match like.as_slice() {
            [] => Ok(None),
            [one] if one.1 == want => Ok(Some(*one)),
            _ => Err(bad("animpack-toml-case").with(
                "names",
                like.iter()
                    .map(|(_, n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            )),
        }
    };
    let found = match pack_file(PACK_FILE)? {
        Some(x) => Some(x),
        None => {
            // The engine's `find_pack_root`: the one top-level folder, when there is exactly one.
            let tops: BTreeSet<&str> = names
                .iter()
                .filter_map(|(_, n, _)| n.split_once('/').map(|(top, _)| top))
                .filter(|top| !top.is_empty())
                .collect();
            match tops.into_iter().collect::<Vec<_>>().as_slice() {
                [one] => pack_file(&format!("{one}/{PACK_FILE}"))?,
                _ => None,
            }
        }
    };
    let Some(&(index, _, size)) = found else {
        return Err(bad("no-animpack-toml"));
    };
    if size > MAX_PACK_FILE {
        return Err(bad("unreadable").with("detail", "animpack.toml is larger than 1 MiB"));
    }
    let mut text = String::new();
    ar.by_index(index)
        .map_err(|e| bad("unreadable").with("detail", e.to_string()))?
        .take(MAX_PACK_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|e| bad("unreadable").with("detail", e.to_string()))?;
    let table: toml::Table = toml::from_str(&text)
        .map_err(|e| bad("unreadable").with("detail", e.message().to_owned()))?;
    let uid = table
        .get("animpack")
        .and_then(|a| a.get("uid"))
        .and_then(toml::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let tail = uid
        .strip_prefix(id)
        .and_then(|t| t.strip_prefix('/'))
        .filter(|t| engine_id(t));
    let Some(tail) = tail else {
        return Err(bad("uid")
            .with("uid", uid.clone())
            .with("want", format!("{id}/<name>")));
    };
    let tombstone = format!("{tail}{TOMBSTONE_EXT}");
    if !is_tombstone_name(&tombstone) {
        return Err(bad("uid")
            .with("uid", uid.clone())
            .with("detail", "its tombstone is no file name on Windows"));
    }
    Ok((uid, tombstone))
}

/// The user root a caller named, or a ledger recorded: an absolute local path with no `..`,
/// judged both by this machine's rules ([`require_local`]) and by its text as Windows reads it
/// ([`windows_text_verdict`]: a share, a device or a verbatim namespace is refused on every
/// machine), before anything touches it.
pub(crate) fn check_root(root: &Path) -> Result<()> {
    // The text first, so a share is `unc` on every machine (on Linux `\\host\x` is otherwise a
    // relative name, and `//host/x` a local one).
    windows_text_verdict(&root.to_string_lossy()).map_err(|reason| escapes(root, reason))?;
    require_local(root)?;
    if root.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(escapes(root, "parent"));
    }
    Ok(())
}

fn inbox_rel(id: &str, name: &str) -> Vec<String> {
    let mut rel: Vec<String> = INBOX_DIRS.iter().map(|s| (*s).to_owned()).collect();
    rel.push(id.to_owned());
    rel.push(name.to_owned());
    rel
}

/// The inbox folder below the canonical `root` as it is now: `Some` when every folder on the way
/// is a real folder, `None` when one is missing; `path-escapes` for a link or a file on the way.
fn existing_inbox(root: &Path, id: &str) -> Result<Option<PathBuf>> {
    let mut cur = root.to_path_buf();
    for c in INBOX_DIRS.iter().copied().chain([id]) {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return Err(escapes(&cur, "link")),
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(escapes(&cur, "not-a-directory")),
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(LauncherError::io_at(&e, &cur)),
        }
    }
    Ok(Some(cur))
}

/// Planning's half ([`crate::plan_install_with_user_pack_root`]): `None` when the package places
/// no pack. Changes nothing on disk.
pub(crate) fn plan(
    pkg: &GiPackage,
    user_root: Option<&Path>,
    game_dir: &Path,
) -> Result<Option<PackInboxPlan>> {
    if pkg.inbox_files.is_empty() {
        return Ok(None);
    }
    let id = &pkg.manifest().integration.id;
    let names: Vec<&str> = pkg.inbox_files.iter().map(InboxPack::name).collect();
    let Some(root) = user_root else {
        return Err(LauncherError::new(codes::PACK_INBOX_UNAVAILABLE)
            .with("reason", "no-user-root")
            .with("files", names.join(",")));
    };
    check_root(root)?;
    let user_root = absolute(root)?;
    let resolved = resolve_lenient(&user_root)?;
    if resolved.starts_with(game_dir) || game_dir.starts_with(&resolved) {
        return Err(escapes(&user_root, "pack-inbox-overlaps-game"));
    }
    match fs::metadata(&user_root) {
        Ok(m) if m.is_dir() => {
            // As the folder is now: no link or file on the way, and nothing but a file (or
            // nothing) where a pack goes. Install checks again as it writes.
            let canon = canonical_dir(&user_root)?;
            if let Some(dir) = existing_inbox(&canon, id)? {
                for name in &names {
                    check_regular_or_absent(&dir.join(name))?;
                }
            }
        }
        Ok(_) => {
            return Err(LauncherError::new(codes::PACK_INBOX_UNAVAILABLE)
                .with("reason", "not-a-directory")
                .with("path", user_root.display().to_string()));
        }
        // The engine makes it at its first start: so does the install.
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(LauncherError::io_at(&e, &user_root)),
    }
    let mut dir = user_root.clone();
    dir.extend(INBOX_DIRS);
    dir.push(id);
    Ok(Some(PackInboxPlan {
        dir: plain(&dir),
        files: pkg
            .inbox_files
            .iter()
            .map(|p| PackInboxFile {
                name: p.name().to_owned(),
                sha256: p.file.sha256.clone(),
                size: p.file.size,
                uid: p.uid.clone(),
                tombstone: p.tombstone.clone(),
            })
            .collect(),
        user_root,
    }))
}

/// The ledger's record of `plan`.
pub(crate) fn ledger_of(plan: &PackInboxPlan) -> LedgerPackInbox {
    LedgerPackInbox {
        user_root: plan.user_root.clone(),
        files: plan
            .files
            .iter()
            .map(|f| LedgerPackFile {
                name: f.name.clone(),
                sha256: f.sha256.clone(),
                tombstone: f.tombstone.clone(),
            })
            .collect(),
    }
}

/// The hold of the profile at the canonical `profile`: its name in the inbox folder.
fn hold_name(profile: &Path) -> String {
    let key = sha256_hex(profile.to_string_lossy().as_bytes());
    format!("{HOLD_PREFIX}{}{HOLD_EXT}", &key[..16])
}

#[derive(Serialize, Deserialize)]
struct Hold {
    schema: u32,
    /// The canonical profile that holds the packs.
    profile: PathBuf,
    /// The tombstones its uninstall would write.
    tombstones: Vec<String>,
}

/// Install's half: the hold first (an uninstall of another profile reading the folder before the
/// first pack lands sees this one holding it), then every pack, each checked against the plan.
pub(crate) fn place(
    pkg: &GiPackage,
    plan: &PackInboxPlan,
    profile: &Path,
    ar: &mut ZipArchive<Cursor<&[u8]>>,
    ctx: &Ctx,
) -> Result<()> {
    let id = &pkg.manifest().integration.id;
    fs::create_dir_all(&plan.user_root).map_err(|e| LauncherError::io_at(&e, &plan.user_root))?;
    let root = canonical_dir(&plan.user_root)?;
    let hold = Hold {
        schema: 1,
        profile: profile.to_path_buf(),
        tombstones: plan.files.iter().map(|f| f.tombstone.clone()).collect(),
    };
    let text = serde_json::to_vec_pretty(&hold).map_err(LauncherError::io)?;
    write_atomic(
        &prepare_target(&root, &inbox_rel(id, &hold_name(profile)))?,
        &text,
    )?;

    let total = pkg.inbox_files.len() as u64;
    for (i, (p, f)) in pkg.inbox_files.iter().zip(&plan.files).enumerate() {
        ctx.check()?;
        if p.name() != f.name || p.file.sha256 != f.sha256 {
            return Err(LauncherError::new(codes::PLAN_STALE).with("detail", "pack-inbox"));
        }
        let bytes = pkg.read(ar, &p.file)?;
        write_atomic(&prepare_target(&root, &inbox_rel(id, &f.name))?, &bytes)?;
        ctx.report("pack-inbox", i as u64 + 1, total);
    }
    Ok(())
}

/// The tombstones the live holds of OTHER profiles in `dir` name. A hold whose profile no longer
/// holds a ledger (removed by hand) holds nothing; one that cannot be read holds nothing.
fn held_elsewhere(dir: &Path, own: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == own || !name.starts_with(HOLD_PREFIX) || !name.ends_with(HOLD_EXT) {
            continue;
        }
        let p = e.path();
        let Ok(m) = fs::symlink_metadata(&p) else {
            continue;
        };
        if !m.is_file() || m.len() > MAX_HOLD {
            continue;
        }
        let Ok(hold) = fs::read(&p).map(|b| serde_json::from_slice::<Hold>(&b)) else {
            continue;
        };
        let Ok(hold) = hold else {
            continue;
        };
        // A file on disk names the profile: judged before it is touched.
        if check_root(&hold.profile).is_err() {
            continue;
        }
        let alive = fs::symlink_metadata(hold.profile.join(LEDGER_FILE)).is_ok_and(|m| m.is_file());
        if alive {
            out.extend(hold.tombstones);
        }
    }
    out
}

/// Whether two user roots are one folder.
fn same_root(a: &Path, b: &Path) -> bool {
    match (resolve_lenient(a), resolve_lenient(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Uninstall's half, for `ledger` of the canonical `profile`: a pack still waiting in the inbox
/// with our digest is removed unless another profile's live hold names it, this profile's hold
/// goes, and each tombstone is written unless `again` (the plan of the update replacing this
/// install) delivers the same pack (by uid) into the same root again, or another profile's live
/// hold names it. `None` when the ledger handed the companion nothing. Asked again with `again`
/// `None` it writes exactly the tombstones `again` held back: what an update that stops before its
/// own ledger exists does ([`crate::install`]).
pub(crate) fn undo(
    profile: &Path,
    ledger: &Ledger,
    again: Option<&PackInboxPlan>,
) -> Result<Option<PackInboxReport>> {
    let Some(inbox) = &ledger.pack_inbox else {
        return Ok(None);
    };
    let mut report = PackInboxReport::default();
    // A ledger is a file on disk: what it names is checked again before anything is touched. A
    // record this launcher never writes is skipped, not refused: refusing would leave an
    // uninstall that can never finish.
    let id = &ledger.integration.id;
    let sound = check_root(&inbox.user_root).is_ok()
        && folder_id_ok(id)
        && inbox
            .files
            .iter()
            .all(|f| is_pack_name(&f.name) && is_tombstone_name(&f.tombstone));
    if !sound {
        report.skipped = Some(codes::PATH_ESCAPES.to_owned());
        return Ok(Some(report));
    }
    let root = match fs::metadata(&inbox.user_root) {
        Ok(m) if m.is_dir() => canonical_dir(&inbox.user_root)?,
        Ok(_) => {
            report.skipped = Some("root-missing".to_owned());
            return Ok(Some(report));
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            report.skipped = Some("root-missing".to_owned());
            return Ok(Some(report));
        }
        Err(e) => return Err(LauncherError::io_at(&e, &inbox.user_root)),
    };
    let through_link = |e: &LauncherError| e.code == codes::PATH_ESCAPES;
    let dir = match existing_inbox(&root, id) {
        Ok(d) => d,
        Err(e) if through_link(&e) => {
            report.skipped = Some(codes::PATH_ESCAPES.to_owned());
            return Ok(Some(report));
        }
        Err(e) => return Err(e),
    };
    let own = hold_name(profile);
    let mut held = BTreeSet::new();
    if let Some(dir) = &dir {
        // Asked first: a pack still waiting that another install delivered too is the one that
        // install waits for (the companion was closed between the two), not ours to remove.
        held = held_elsewhere(dir, &own);
        for f in &inbox.files {
            if held.contains(&f.tombstone) {
                continue;
            }
            let p = dir.join(&f.name);
            match file_state(&p) {
                Ok(FileState::File { sha256 }) if sha256 == f.sha256 => {
                    fs::remove_file(&p).map_err(|e| LauncherError::io_at(&e, &p))?;
                    report.removed.push(f.name.clone());
                }
                // Changed since, or not there (imported): not ours to remove.
                Ok(_) => {}
                // A link or a folder in its place: never followed.
                Err(e) if through_link(&e) => {}
                Err(e) => return Err(e),
            }
        }
        let p = dir.join(&own);
        if fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            fs::remove_file(&p).map_err(|e| LauncherError::io_at(&e, &p))?;
        }
    }
    let again = again.filter(|a| same_root(&a.user_root, &inbox.user_root));
    for f in &inbox.files {
        if again.is_some_and(|a| a.files.iter().any(|n| n.tombstone == f.tombstone)) {
            continue;
        }
        if held.contains(&f.tombstone) {
            report.held.push(f.tombstone.clone());
            continue;
        }
        let target = match prepare_target(&root, &inbox_rel(id, &f.tombstone)) {
            Ok(t) => t,
            Err(e) if through_link(&e) => {
                report.skipped = Some(codes::PATH_ESCAPES.to_owned());
                return Ok(Some(report));
            }
            Err(e) => return Err(e),
        };
        write_atomic(&target, b"")?;
        report.tombstones.push(f.tombstone.clone());
    }
    Ok(Some(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inbox_folder_and_a_tail_are_what_the_engine_reads() {
        for ok in ["astra.peak", "a", "a-b_c.d", "0", &"a".repeat(64)] {
            assert!(engine_id(ok), "{ok}");
        }
        for bad in [
            "",
            ".hidden",
            "..",
            "Astra.Peak",
            "a/b",
            "a b",
            "a:b",
            &"a".repeat(65),
        ] {
            assert!(!engine_id(bad), "{bad}");
        }
        // A trailing dot Windows drops, a device name Windows opens: never a folder of ours.
        for bad in ["a.", "con", "nul", "com1", "lpt9"] {
            assert!(!folder_id_ok(bad), "{bad}");
        }
    }

    #[test]
    fn only_an_astrapack_goes_into_the_inbox() {
        for ok in ["climb.astrapack", "peak-climb.ASTRAPACK", "a.b.astrapack"] {
            assert!(is_pack_name(ok), "{ok}");
        }
        for bad in [
            ".astrapack",
            ".hidden.astrapack",
            "climb.zip",
            "climb.remove",
            "climb.astrapack.part",
            "con.astrapack",
            "a:b.astrapack",
            "climb.astrapack.",
        ] {
            assert!(!is_pack_name(bad), "{bad}");
        }
        assert!(is_tombstone_name("climb.remove"));
        // The tail is the engine's: no capital, never dot-led, never a device on Windows.
        for bad in [
            "Climb.remove",
            ".x.remove",
            "con.remove",
            "nul.remove",
            "climb.REMOVE",
        ] {
            assert!(!is_tombstone_name(bad), "{bad}");
        }
    }

    /// The user root as Windows reads its text, on every machine: a share, a device, a verbatim
    /// namespace or a drive-relative path is refused before anything touches it; `..` too.
    #[test]
    fn a_user_root_is_a_local_absolute_folder_on_every_machine() {
        let reason = |s: &str| {
            check_root(Path::new(s))
                .unwrap_err()
                .param("reason")
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!(reason(r"\\server\share\pack"), "unc");
        assert_eq!(reason("//server/share/pack"), "unc");
        assert_eq!(reason(r"\\?\UNC\server\share\pack"), "verbatim-unc");
        assert_eq!(reason(r"\\.\pipe\pack"), "device");
        assert_eq!(reason(r"\\?\GLOBALROOT\Device\x"), "verbatim");
        assert_eq!(reason("C:pack"), "not-absolute");
        assert_eq!(reason("relative/pack"), "not-absolute");
        if cfg!(windows) {
            assert!(
                check_root(Path::new(
                    r"C:\Users\me\AppData\Roaming\Astra\companion\pack"
                ))
                .is_ok()
            );
            assert!(check_root(Path::new(r"\\?\C:\Users\me\pack")).is_ok());
            assert_eq!(reason(r"C:\Users\me\..\pack"), "parent");
        } else {
            assert!(check_root(Path::new("/home/me/.config/astra/companion/pack")).is_ok());
            assert_eq!(reason("/home/me/../pack"), "parent");
            // `C:\…` is a relative name here, never a drive.
            assert_eq!(reason(r"C:\Users\me\pack"), "not-absolute");
        }
    }
}
