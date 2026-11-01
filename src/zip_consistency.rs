// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A zip that two readers read as two archives ("zip cloaking", the class of zip < 9.0.5's
//! advisory) is refused before anything is read out of it: [`check`], the first thing
//! [`crate::GiPackage::open`] asks of a zip once it has its digest and its entry count.
//!
//! zip 9 reads every entry through the central directory, so a local header, a data descriptor or
//! a deflate stream that says something else changes nothing it reads — and everything a reader
//! that walks the local headers reads: a catalogue's ingest, `unzip` from a pipe, libarchive (so
//! bsdtar and Windows' `tar.exe`). The checks are those of Astra's own installer
//! (`ops::install_plugin::refuse_inconsistent_headers`) and of AstraPlugins'
//! `testdata/zip-cloaking/` (whose three vectors `tests/fixtures/zip-cloaking/` holds by digest):
//!
//! 1. zip 9's `validate_header_consistency`: every local header against its central record (name,
//!    method, encryption and data-descriptor flags, CRC and sizes, a descriptor's zeros allowed);
//!    no two records with one name; no two entries overlapping; no local header the directory
//!    does not list in the bytes between entries.
//! 2. No Unicode Path extra field (0x7075) in either header of any entry: it renames the entry for
//!    the readers that honour it (zip 9 among them) and for no others, and both headers may carry
//!    the same one, so (1) passes it. The raw blocks are read, not zip's parse of them, which
//!    keeps a field only when its CRC matches.
//! 3. A name every reader decodes alike: ASCII, or flagged UTF-8 (general-purpose bit 11) in both
//!    headers and UTF-8 indeed. Without the flag, zip 9 reads valid UTF-8 as UTF-8 and zip 8 (so
//!    launcher 0.3.1, Astra 0.2.7's) as CP437, and Windows' own reader in the machine's OEM code
//!    page: one entry, three names.
//! 4. Every Deflated entry is one deflate stream that ends on the last of the bytes the directory
//!    gives it and inflates to exactly its declared size and CRC. A reader of local headers ends a
//!    deflated entry where its stream ends; a stream that ends early leaves slack inside the
//!    entry, where (with bit 3, sizes in the directory only) a fake descriptor and a local header
//!    hide a whole entry from that reader while (1) sees nothing wrong (`slack.zip`). Checked on
//!    every deflated entry, bit 3 or not, ZIP64 or not: which local sizes a reader believes
//!    depends on the reader. The inflater is drained after its last input byte, so a stream that
//!    owes output past that byte (65,537 zeros behind one byte) is whole.
//! 5. An entry with bit 3, Stored or Deflated, is followed exactly at its recorded end by a
//!    descriptor WITH its signature whose CRC and sizes are the record's (64-bit sizes when its
//!    local header carries a ZIP64 field): a reader of local headers reads the descriptor where the
//!    entry ends for it, and one that is not there makes it read the next header out of step. A
//!    Stored entry's data also holds no signature followed by the CRC of the bytes before it: a
//!    scanning reader ends such an entry at the first signature whose CRC matches what it has read
//!    (`stored-dd.zip`). The signature is required, where AstraPlugins' README calls it optional:
//!    a scanning reader cannot find a descriptor without one (debt row 34).
//! 6. A recorded range inside the file: data that runs past its end is refused, never read short.
//!
//! An encrypted entry, or one of another method, is not inflated here: the hygiene loop that
//! follows in `open` refuses it (`zip-unsafe-entry`) before anything is read either. Bit 3 is
//! accepted because ordinary tools write it (Info-ZIP and Python writing to a pipe:
//! `tests/fixtures/zip-writers/`), and the work is bounded by `max_inflated` over all entries
//! together, so the check is not the bomb it looks for.
//!
//! This is the list of differentials known here, not a proof that every ZIP reader agrees with
//! this one about every archive (debt row 35).

use std::io::Cursor;

use flate2::{Decompress, FlushDecompress, Status};
use zip::{CompressionMethod, ZipArchive};

use crate::error::{LauncherError, Result, codes};

const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
/// The data-descriptor signature, `PK\x07\x08`.
const DESCRIPTOR_SIGNATURE: [u8; 4] = 0x0807_4b50u32.to_le_bytes();
const ZIP64_FIELD: u16 = 0x0001;
const UNICODE_PATH_FIELD: u16 = 0x7075;
const FLAG_DESCRIPTOR: u16 = 1 << 3;
const FLAG_UTF8: u16 = 1 << 11;

/// One entry as the central directory records it, and where its local header puts its data.
struct Entry {
    name: String,
    method: CompressionMethod,
    encrypted: bool,
    descriptor: bool,
    local_zip64: bool,
    data_start: usize,
    compressed: u64,
    size: u64,
    crc: u32,
}

fn refusal(entry: &str, reason: &str) -> LauncherError {
    LauncherError::new(codes::ZIP_INCONSISTENT)
        .with("entry", entry)
        .with("reason", reason)
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The ids of an extra block's fields; `None` when a field runs past the block.
fn extra_ids(block: &[u8]) -> Option<Vec<u16>> {
    let mut ids = Vec::new();
    let mut at = 0;
    while at < block.len() {
        let id = u16_at(block, at)?;
        let len = usize::from(u16_at(block, at + 2)?);
        at = at.checked_add(4 + len)?;
        if at > block.len() {
            return None;
        }
        ids.push(id);
    }
    Some(ids)
}

/// The header at `at`: (flags, name, extra block), for a local header (`central` false: 30 fixed
/// bytes) or a central one (46).
fn header(zip: &[u8], at: u64, central: bool) -> Option<(u16, &[u8], &[u8])> {
    let at = usize::try_from(at).ok()?;
    let (signature, flags, lens, fixed) = if central {
        (CENTRAL_SIGNATURE, 8, 28, 46)
    } else {
        (LOCAL_SIGNATURE, 6, 26, 30)
    };
    if u32_at(zip, at)? != signature {
        return None;
    }
    let name_len = usize::from(u16_at(zip, at + lens)?);
    let extra_len = usize::from(u16_at(zip, at + lens + 2)?);
    let name_at = at.checked_add(fixed)?;
    let extra_at = name_at.checked_add(name_len)?;
    let name = zip.get(name_at..extra_at)?;
    let extra = zip.get(extra_at..extra_at.checked_add(extra_len)?)?;
    Some((u16_at(zip, at + flags)?, name, extra))
}

/// Checks (2), (3) and (6) for entry `i`, and what (4) and (5) need of it.
fn entry(ar: &mut ZipArchive<Cursor<&[u8]>>, zip: &[u8], i: usize) -> Result<Entry> {
    let f = ar.by_index_raw(i).map_err(|e| {
        refusal("", "headers")
            .with("index", i.to_string())
            .with("detail", e.to_string())
    })?;
    let lossy = String::from_utf8_lossy(f.name_raw()).into_owned();
    let (central_flags, central_name, central_extra) =
        header(zip, f.central_header_start(), true).ok_or_else(|| refusal(&lossy, "truncated"))?;
    let (local_flags, local_name, local_extra) =
        header(zip, f.header_start(), false).ok_or_else(|| refusal(&lossy, "truncated"))?;
    let name = String::from_utf8_lossy(central_name).into_owned();
    let central_ids = extra_ids(central_extra).ok_or_else(|| refusal(&name, "extra-field"))?;
    let local_ids = extra_ids(local_extra).ok_or_else(|| refusal(&name, "extra-field"))?;
    if central_ids.contains(&UNICODE_PATH_FIELD) || local_ids.contains(&UNICODE_PATH_FIELD) {
        return Err(refusal(&name, "unicode-path"));
    }
    if !central_name.is_ascii()
        && (central_flags & FLAG_UTF8 == 0
            || local_flags & FLAG_UTF8 == 0
            || std::str::from_utf8(central_name).is_err()
            || local_name != central_name)
    {
        return Err(refusal(&name, "name-encoding"));
    }
    // Where the local header puts the data: zip reads it from there too.
    let data_start = usize::try_from(f.header_start())
        .ok()
        .and_then(|h| h.checked_add(30 + local_name.len() + local_extra.len()))
        .ok_or_else(|| refusal(&name, "truncated"))?;
    if f.data_start().is_some_and(|d| d != data_start as u64) {
        return Err(refusal(&name, "headers").with("detail", "data-start"));
    }
    let end = u64::try_from(data_start)
        .ok()
        .and_then(|d| d.checked_add(f.compressed_size()));
    if end.is_none_or(|end| end > zip.len() as u64) {
        return Err(refusal(&name, "truncated"));
    }
    Ok(Entry {
        method: f.compression(),
        encrypted: f.encrypted(),
        descriptor: central_flags & FLAG_DESCRIPTOR != 0,
        local_zip64: local_ids.contains(&ZIP64_FIELD),
        data_start,
        compressed: f.compressed_size(),
        size: f.size(),
        crc: f.crc32(),
        name,
    })
}

/// Refuses `zip` (open as `ar`) unless every reader reads one archive in it: the module's checks.
pub(crate) fn check(
    ar: &mut ZipArchive<Cursor<&[u8]>>,
    zip: &[u8],
    max_inflated: u64,
) -> Result<()> {
    ar.validate_header_consistency()
        .map_err(|e| refusal("", "headers").with("detail", e.to_string()))?;
    let mut budget = Budget {
        left: max_inflated,
        limit: max_inflated,
    };
    for i in 0..ar.len() {
        let e = entry(ar, zip, i)?;
        if e.encrypted {
            continue;
        }
        match (e.method, e.descriptor) {
            (CompressionMethod::Deflated, descriptor) => {
                deflate_ends_on_its_record(zip, &e, &mut budget)?;
                if descriptor {
                    descriptor_at_its_end(zip, &e)?;
                }
            }
            (CompressionMethod::Stored, true) => {
                descriptor_at_its_end(zip, &e)?;
                stored_holds_no_descriptor(zip, &e, &mut budget)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// What the checks may still inflate or scan, over all entries.
struct Budget {
    left: u64,
    limit: u64,
}

impl Budget {
    /// `too-large`, as `open`'s own count of declared sizes says it, with the entry the checks
    /// were reading when the budget ran out.
    fn spend(&mut self, n: u64, entry: &str) -> Result<()> {
        self.left = self.left.checked_sub(n).ok_or_else(|| {
            LauncherError::new(codes::TOO_LARGE)
                .with("uncompressed", "true")
                .with("limit", self.limit.to_string())
                .with("entry", entry)
        })?;
        Ok(())
    }
}

/// The entry's data: `compressed` bytes from `data_start`, inside the file ([`entry`] checked).
fn data<'z>(zip: &'z [u8], e: &Entry) -> &'z [u8] {
    &zip[e.data_start..e.data_start + e.compressed as usize]
}

/// Check (4): one raw deflate stream, ending on the last recorded byte, inflating to exactly `size`
/// bytes with the recorded CRC. Inflates into a scratch buffer, never to disk.
fn deflate_ends_on_its_record(zip: &[u8], e: &Entry, budget: &mut Budget) -> Result<()> {
    let raw = data(zip, e);
    let mut inflater = Decompress::new(false);
    let mut out = vec![0u8; 64 * 1024];
    let mut crc = crc32fast::Hasher::new();
    loop {
        let (was_in, was_out) = (inflater.total_in(), inflater.total_out());
        let status = inflater
            .decompress(&raw[was_in as usize..], &mut out, FlushDecompress::None)
            .map_err(|_| refusal(&e.name, "deflate-invalid"))?;
        let produced = inflater.total_out() - was_out;
        crc.update(&out[..produced as usize]);
        budget.spend(produced, &e.name)?;
        if inflater.total_out() > e.size {
            return Err(refusal(&e.name, "deflate-size").with("declared", e.size.to_string()));
        }
        if status == Status::StreamEnd {
            break;
        }
        // Neither input taken nor output given: every byte is in and the stream has not ended.
        if inflater.total_in() == was_in && produced == 0 {
            return Err(
                refusal(&e.name, "deflate-unended").with("recorded", e.compressed.to_string())
            );
        }
    }
    if inflater.total_in() != e.compressed {
        return Err(refusal(&e.name, "deflate-end")
            .with("at", inflater.total_in().to_string())
            .with("recorded", e.compressed.to_string()));
    }
    if inflater.total_out() != e.size {
        return Err(refusal(&e.name, "deflate-size").with("declared", e.size.to_string()));
    }
    if crc.finalize() != e.crc {
        return Err(refusal(&e.name, "crc"));
    }
    Ok(())
}

/// Check (5), first half: a signed descriptor with the record's CRC and sizes exactly at the data's
/// end.
fn descriptor_at_its_end(zip: &[u8], e: &Entry) -> Result<()> {
    let end = e.data_start + e.compressed as usize;
    let width = if e.local_zip64 { 8 } else { 4 };
    let size_at = |at: usize| {
        if width == 8 {
            u64_at(zip, at)
        } else {
            u32_at(zip, at).map(u64::from)
        }
    };
    let found = zip.get(end..end + 4) == Some(&DESCRIPTOR_SIGNATURE[..])
        && u32_at(zip, end + 4) == Some(e.crc)
        && size_at(end + 8) == Some(e.compressed)
        && size_at(end + 8 + width) == Some(e.size);
    if !found {
        return Err(refusal(&e.name, "descriptor-missing"));
    }
    Ok(())
}

/// Check (5), second half, for a Stored entry: no descriptor a scanning reader would stop at
/// first inside the data, and the data's own CRC the record's. Runs after
/// [`descriptor_at_its_end`], so the descriptor's 16 bytes follow the data. One pass: the running
/// CRC moves forward to each signature, never back.
fn stored_holds_no_descriptor(zip: &[u8], e: &Entry, budget: &mut Budget) -> Result<()> {
    if e.compressed != e.size {
        return Err(refusal(&e.name, "stored-size"));
    }
    let end = e.data_start + e.compressed as usize;
    budget.spend(e.size, &e.name)?;
    // Every position inside the data whose 8 bytes (signature, CRC) are in the file: the last
    // ones run on into the real descriptor, as a scanning reader reads them.
    let mut running = crc32fast::Hasher::new();
    let mut hashed_to = e.data_start;
    let mut p = e.data_start;
    while p < end {
        let Some(off) = zip[p..end]
            .iter()
            .position(|&b| b == DESCRIPTOR_SIGNATURE[0])
        else {
            break;
        };
        p += off;
        if zip[p..p + 4] == DESCRIPTOR_SIGNATURE {
            running.update(&zip[hashed_to..p]);
            hashed_to = p;
            if Some(running.clone().finalize()) == u32_at(zip, p + 4) {
                return Err(refusal(&e.name, "descriptor-inside")
                    .with("at", (p - e.data_start).to_string()));
            }
        }
        p += 1;
    }
    running.update(&zip[hashed_to..end]);
    if running.finalize() != e.crc {
        return Err(refusal(&e.name, "crc"));
    }
    Ok(())
}
