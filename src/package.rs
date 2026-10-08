// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `astra-gi.zip`: verified against its SHA-256, every entry checked, the manifest read, and
//! the files the manifest names resolved. Only those files are ever extracted.

use std::collections::HashSet;
use std::io::{Cursor, Read};

use serde::Serialize;
use zip::{CompressionMethod, ZipArchive};

use crate::digest::{normalize_hex, sha256_hex};
use crate::error::{LauncherError, Result, codes};
use crate::manifest::{
    FromPattern, IntegrationInfo, MANIFEST_FILE, Manifest, Target, from_pattern, safe_component,
};
use crate::paths::{is_reserved, parse_jailed};

/// The caps a package must stay under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub max_zip_bytes: u64,
    pub max_uncompressed_bytes: u64,
    pub max_entries: usize,
    pub max_manifest_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_zip_bytes: 256 * 1024 * 1024,
            max_uncompressed_bytes: 1024 * 1024 * 1024,
            max_entries: 20_000,
            max_manifest_bytes: 1024 * 1024,
        }
    }
}

/// One file of the package and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackFile {
    pub index: usize,
    pub entry: String,
    /// Destination components below the profile, or below the exe's folder.
    pub rel: Vec<String>,
    pub size: u64,
    pub sha256: String,
}

impl PackFile {
    pub fn rel_string(&self) -> String {
        self.rel.join("/")
    }
}

/// A verified package: its manifest and the files that manifest names.
pub struct GiPackage {
    zip: Vec<u8>,
    sha256: String,
    manifest: Manifest,
    manifest_text: String,
    pub(crate) profile_files: Vec<PackFile>,
    pub(crate) game_files: Vec<PackFile>,
}

impl std::fmt::Debug for GiPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GiPackage")
            .field("sha256", &self.sha256)
            .field("integration", &self.manifest.integration.id)
            .finish_non_exhaustive()
    }
}

fn unsafe_entry(entry: &str, reason: &str) -> LauncherError {
    LauncherError::new(codes::ZIP_UNSAFE_ENTRY)
        .with("entry", entry)
        .with("reason", reason)
}

/// Normalizes an entry name and refuses absolute paths, `..`, drive letters and the like.
/// Returns the `/`-joined name without a trailing slash, and whether it is a folder.
fn entry_name(name: &str) -> Result<(String, bool)> {
    let n = name.replace('\\', "/");
    if n.starts_with('/') {
        return Err(unsafe_entry(name, "absolute"));
    }
    let is_dir = n.ends_with('/');
    let body = n.strip_suffix('/').unwrap_or(&n);
    if body.is_empty() {
        return Err(unsafe_entry(name, "empty"));
    }
    for c in body.split('/') {
        if c == ".." {
            return Err(unsafe_entry(name, "parent"));
        }
        if !safe_component(c) {
            return Err(unsafe_entry(name, "name"));
        }
    }
    Ok((body.to_owned(), is_dir))
}

const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

/// What `inspect` prints: the manifest as a consent sheet would show it, before any game is named.
#[derive(Debug, Clone, Serialize)]
pub struct PackageSummary {
    pub sha256: String,
    pub schema: u32,
    pub integration: IntegrationInfo,
    pub target: Target,
    pub profile_files: u32,
    pub profile_bytes: u64,
    pub game_files: Vec<SummaryGameFile>,
    pub launch_args: Vec<String>,
    pub env: Vec<String>,
    pub proton_override: Option<(String, String)>,
    pub config_writes: Vec<SummaryConfigWrite>,
    pub variables: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryGameFile {
    pub rel_path: String,
    pub sha256: String,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryConfigWrite {
    pub file: String,
    pub section: String,
    pub keys: Vec<String>,
}

impl GiPackage {
    /// Verifies the zip against `expected_sha256` and the default [`Limits`], then reads it.
    pub fn open(zip: &[u8], expected_sha256: &str) -> Result<GiPackage> {
        GiPackage::open_with_limits(zip, expected_sha256, &Limits::default())
    }

    pub fn open_with_limits(
        zip: &[u8],
        expected_sha256: &str,
        limits: &Limits,
    ) -> Result<GiPackage> {
        if zip.len() as u64 > limits.max_zip_bytes {
            return Err(LauncherError::new(codes::TOO_LARGE)
                .with("bytes", zip.len().to_string())
                .with("limit", limits.max_zip_bytes.to_string()));
        }
        let got = sha256_hex(zip);
        let want = normalize_hex(expected_sha256);
        if want.as_deref() != Some(got.as_str()) {
            return Err(LauncherError::new(codes::DIGEST_MISMATCH)
                .with("want", expected_sha256)
                .with("got", got));
        }

        let mut ar = ZipArchive::new(Cursor::new(zip))
            .map_err(|e| unsafe_entry("", "not-a-zip").with("detail", e.to_string()))?;
        if ar.len() > limits.max_entries {
            return Err(LauncherError::new(codes::ZIP_TOO_MANY_ENTRIES)
                .with("entries", ar.len().to_string())
                .with("limit", limits.max_entries.to_string()));
        }

        // Every entry is checked, extracted or not.
        let mut files: Vec<(usize, String, u64)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut total: u64 = 0;
        for i in 0..ar.len() {
            let f = ar
                .by_index_raw(i)
                .map_err(|e| unsafe_entry("", "corrupt").with("detail", e.to_string()))?;
            let raw_name = f.name().to_owned();
            if f.name_raw().contains(&0) {
                return Err(unsafe_entry(&raw_name, "name"));
            }
            let (name, mut is_dir) = entry_name(&raw_name)?;
            if let Some(mode) = f.unix_mode() {
                match mode & S_IFMT {
                    S_IFLNK => return Err(unsafe_entry(&raw_name, "symlink")),
                    S_IFDIR => is_dir = true,
                    0 | S_IFREG => {}
                    _ => return Err(unsafe_entry(&raw_name, "special-file")),
                }
            }
            if f.is_symlink() {
                return Err(unsafe_entry(&raw_name, "symlink"));
            }
            if f.encrypted() {
                return Err(unsafe_entry(&raw_name, "encrypted"));
            }
            if !matches!(
                f.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            ) {
                return Err(unsafe_entry(&raw_name, "compression"));
            }
            if !seen.insert(name.to_lowercase()) {
                return Err(unsafe_entry(&raw_name, "duplicate"));
            }
            total = total.saturating_add(f.size());
            if total > limits.max_uncompressed_bytes {
                return Err(LauncherError::new(codes::TOO_LARGE)
                    .with("uncompressed", "true")
                    .with("limit", limits.max_uncompressed_bytes.to_string()));
            }
            if !is_dir {
                files.push((i, name, f.size()));
            }
        }

        let Some(&(mi, _, msize)) = files.iter().find(|(_, n, _)| n == MANIFEST_FILE) else {
            return Err(LauncherError::new(codes::MANIFEST_MISSING));
        };
        if msize > limits.max_manifest_bytes {
            return Err(LauncherError::new(codes::TOO_LARGE)
                .with("entry", MANIFEST_FILE)
                .with("limit", limits.max_manifest_bytes.to_string()));
        }
        let manifest_bytes = read_entry(&mut ar, mi, MANIFEST_FILE, msize)?;
        let manifest_text = String::from_utf8(manifest_bytes)
            .map_err(|_| LauncherError::new(codes::MANIFEST_INVALID).with("detail", "not UTF-8"))?;
        let manifest = Manifest::parse(&manifest_text)?;

        let not_in_zip = |key: String| {
            LauncherError::new(codes::MANIFEST_INVALID)
                .with("key", key)
                .with("detail", "not in the zip")
        };

        let mut profile_files: Vec<PackFile> = Vec::new();
        for (n, rule) in manifest.files.iter().enumerate() {
            let dest = parse_jailed(&rule.to)?;
            let before = profile_files.len();
            match from_pattern(&rule.from, &format!("files[{n}].from"))? {
                FromPattern::Exact(p) => {
                    let &(index, ref entry, size) = files
                        .iter()
                        .find(|(_, e, _)| *e == p)
                        .ok_or_else(|| not_in_zip(format!("files[{n}].from")))?;
                    let mut rel = dest.rel.clone();
                    if dest.is_dir {
                        rel.push(entry.rsplit('/').next().unwrap_or(entry).to_owned());
                    }
                    profile_files.push(PackFile {
                        index,
                        entry: entry.clone(),
                        rel,
                        size,
                        sha256: String::new(),
                    });
                }
                FromPattern::Below(prefix) => {
                    for (index, entry, size) in &files {
                        if entry == MANIFEST_FILE {
                            continue;
                        }
                        if let Some(rest) = entry.strip_prefix(prefix.as_str()) {
                            let mut rel = dest.rel.clone();
                            rel.extend(rest.split('/').map(str::to_owned));
                            profile_files.push(PackFile {
                                index: *index,
                                entry: entry.clone(),
                                rel,
                                size: *size,
                                sha256: String::new(),
                            });
                        }
                    }
                }
            }
            if profile_files.len() == before {
                return Err(not_in_zip(format!("files[{n}].from")));
            }
        }

        let mut game_files: Vec<PackFile> = Vec::new();
        for (n, rule) in manifest.game_files.iter().enumerate() {
            let dest = manifest.game_file_dest(n);
            let FromPattern::Exact(p) = from_pattern(&rule.from, &format!("game_files[{n}].from"))?
            else {
                unreachable!("checked at parse");
            };
            let &(index, ref entry, size) = files
                .iter()
                .find(|(_, e, _)| *e == p)
                .ok_or_else(|| not_in_zip(format!("game_files[{n}].from")))?;
            game_files.push(PackFile {
                index,
                entry: entry.clone(),
                rel: dest.rel,
                size,
                sha256: String::new(),
            });
        }

        // A `**` mapping joins the zip's own names to the folder: check the joined result, so no
        // entry can land on the ledger, its lock or the backups.
        for f in &profile_files {
            if f.rel.first().is_some_and(|c| is_reserved(c)) {
                return Err(LauncherError::new(codes::PATH_ESCAPES)
                    .with("path", format!("${{profile}}/{}", f.rel_string()))
                    .with("reason", "reserved"));
            }
        }

        for set in [&profile_files, &game_files] {
            let mut dests: HashSet<String> = HashSet::new();
            for f in set {
                if !dests.insert(f.rel_string().to_lowercase()) {
                    return Err(LauncherError::new(codes::MANIFEST_INVALID)
                        .with("key", "files")
                        .with("detail", format!("two sources for {}", f.rel_string())));
                }
            }
        }

        // Read every named file once: the CRC and the real size are checked, and the digest kept.
        for f in profile_files.iter_mut().chain(game_files.iter_mut()) {
            let bytes = read_entry(&mut ar, f.index, &f.entry, f.size)?;
            f.sha256 = sha256_hex(&bytes);
        }
        for (n, f) in game_files.iter().enumerate() {
            if let Some(pinned) = &manifest.game_files[n].sha256
                && normalize_hex(pinned).as_deref() != Some(f.sha256.as_str())
            {
                return Err(LauncherError::new(codes::DIGEST_MISMATCH)
                    .with("file", f.rel_string())
                    .with("want", pinned.clone())
                    .with("got", f.sha256.clone()));
            }
        }

        Ok(GiPackage {
            zip: zip.to_vec(),
            sha256: got,
            manifest,
            manifest_text,
            profile_files,
            game_files,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The manifest exactly as the zip carries it.
    pub fn manifest_text(&self) -> &str {
        &self.manifest_text
    }

    /// The zip's SHA-256, lowercase hex.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn summary(&self) -> PackageSummary {
        let m = &self.manifest;
        PackageSummary {
            sha256: self.sha256.clone(),
            schema: m.schema,
            integration: m.integration.clone(),
            target: m.target.clone(),
            profile_files: self.profile_files.len() as u32,
            profile_bytes: self.profile_files.iter().map(|f| f.size).sum(),
            game_files: self
                .game_files
                .iter()
                .enumerate()
                .map(|(n, f)| SummaryGameFile {
                    rel_path: f.rel_string(),
                    sha256: f.sha256.clone(),
                    pinned: m.game_files[n].sha256.is_some(),
                })
                .collect(),
            launch_args: m.launch.args.clone(),
            env: m.launch.env.keys().cloned().collect(),
            proton_override: m.proton_override(),
            config_writes: m
                .config_writes
                .iter()
                .map(|c| SummaryConfigWrite {
                    file: c.file.clone(),
                    section: c.section.clone(),
                    keys: c.set.keys().cloned().collect(),
                })
                .collect(),
            variables: m.variables().into_iter().collect(),
        }
    }

    pub(crate) fn archive(&self) -> Result<ZipArchive<Cursor<&[u8]>>> {
        ZipArchive::new(Cursor::new(self.zip.as_slice()))
            .map_err(|e| unsafe_entry("", "not-a-zip").with("detail", e.to_string()))
    }

    /// Reads one named file again and checks it against the digest taken at `open`.
    pub(crate) fn read(&self, ar: &mut ZipArchive<Cursor<&[u8]>>, f: &PackFile) -> Result<Vec<u8>> {
        let bytes = read_entry(ar, f.index, &f.entry, f.size)?;
        if sha256_hex(&bytes) != f.sha256 {
            return Err(LauncherError::new(codes::DIGEST_MISMATCH).with("file", f.rel_string()));
        }
        Ok(bytes)
    }
}

/// Reads one entry, never more than it declares: a header that lies about its size is refused.
fn read_entry(
    ar: &mut ZipArchive<Cursor<&[u8]>>,
    index: usize,
    name: &str,
    declared: u64,
) -> Result<Vec<u8>> {
    let f = ar
        .by_index(index)
        .map_err(|e| unsafe_entry(name, "corrupt").with("detail", e.to_string()))?;
    let mut buf = Vec::with_capacity(declared.min(64 * 1024 * 1024) as usize);
    f.take(declared.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| unsafe_entry(name, "corrupt").with("detail", e.to_string()))?;
    if buf.len() as u64 != declared {
        return Err(unsafe_entry(name, "size"));
    }
    Ok(buf)
}
