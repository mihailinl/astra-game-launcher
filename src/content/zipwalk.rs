// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The content catalogue's own archive reader, `walk_zip` (AstraPlugins
//! `tools/check-content-goldens.py`; `spec/content/ingest-v1.md` §3 steps 1–7), which
//! `spec/games/manifest.md` §2.7.1 and §2.7.3 have a game integration's embedded bundles AND its
//! own archives read with — every cloaking check included — so that what the gate reads is what
//! the content catalogue reads, byte for byte. It reads raw headers, never the `zip` crate's
//! index: a reader that trusts one of the two directories is what a cloaked archive fools.
//!
//! The steps run in the contract's order, each over every entry before the next begins, and the
//! first that fails decides the code ([`Refusal`]): the end record (`E_ZIP_END_RECORD`, zip64 in
//! any spelling included), the entry count (`E_ARCHIVE_TOO_MANY_ENTRIES`), a zip64 pre-pass and
//! then each directory entry (`E_ARCHIVE_ENCRYPTED`, `E_ARCHIVE_METHOD`, `E_ARCHIVE_LINK`,
//! `E_ZIP_HEADERS_INCONSISTENT`: the headers agree, the entries tile the file, the directory is in
//! the file's order), the names (`E_ARCHIVE_NAME_UNSAFE`), the sizes and the 32 MiB of glTF JSON
//! (`E_ARCHIVE_TOO_LARGE`, `E_ARCHIVE_RATIO`), Stored-only types (`E_ARCHIVE_MUST_BE_STORED`) and
//! the raw read (`E_ZIP_HEADERS_INCONSISTENT`). Where the reference's own reader raises instead of
//! refusing (a directory cut short, a deflate stream that is not one), the walk says so
//! ([`WalkError::Unreadable`]), and each caller answers as the reference's caller does.
//!
//! Two sets of limits ([`WalkLimits`]): a content bundle's ([`BUNDLE`]: 4,096 entries, 128 MiB,
//! Stored-only images and models) and a game's own archive's ([`GAME_ARCHIVE`]: 65,536 entries,
//! 512 MiB, no Stored-only rule; manifest.md §2.7.1).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use flate2::{Decompress, FlushDecompress, Status};
use unicode_normalization::UnicodeNormalization;

/// The content contract's codes this reader refuses with.
pub(crate) const E_ZIP_END_RECORD: &str = "E_ZIP_END_RECORD";
pub(crate) const E_ZIP_HEADERS_INCONSISTENT: &str = "E_ZIP_HEADERS_INCONSISTENT";
pub(crate) const E_ARCHIVE_TOO_MANY_ENTRIES: &str = "E_ARCHIVE_TOO_MANY_ENTRIES";
pub(crate) const E_ARCHIVE_ENCRYPTED: &str = "E_ARCHIVE_ENCRYPTED";
pub(crate) const E_ARCHIVE_METHOD: &str = "E_ARCHIVE_METHOD";
pub(crate) const E_ARCHIVE_LINK: &str = "E_ARCHIVE_LINK";
pub(crate) const E_ARCHIVE_NAME_UNSAFE: &str = "E_ARCHIVE_NAME_UNSAFE";
pub(crate) const E_ARCHIVE_TOO_LARGE: &str = "E_ARCHIVE_TOO_LARGE";
pub(crate) const E_ARCHIVE_RATIO: &str = "E_ARCHIVE_RATIO";
pub(crate) const E_ARCHIVE_MUST_BE_STORED: &str = "E_ARCHIVE_MUST_BE_STORED";

const MIB: u64 = 1024 * 1024;
/// One GLB JSON chunk, or one `.gltf` (ingest-v1.md §3 step 10).
pub(crate) const MAX_GLTF_JSON: u64 = 16 * MIB;
/// Every GLB JSON chunk plus every `.gltf` of one archive (step 5) — and, for a game integration,
/// of one release (manifest.md §2.7.3).
pub(crate) const MAX_GLTF_JSON_TOTAL: u64 = 32 * MIB;
const MAX_RATIO: u64 = 100;
const MAX_NAME: usize = 240;
const MAX_SEGMENTS: usize = 16;
const STORED_ONLY: &[&str] = &[".vrm", ".glb", ".png", ".jpg", ".webp", ".ktx2"];
pub(crate) const GLB_MAGIC: &[u8] = b"glTF";

const CEN: &[u8] = b"PK\x01\x02";
const LOC: &[u8] = b"PK\x03\x04";
const END: &[u8] = b"PK\x05\x06";
const DESCRIPTOR: &[u8] = b"PK\x07\x08";

/// What one archive may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WalkLimits {
    pub max_entries: u64,
    pub max_total: u64,
    pub stored_only: bool,
}

/// A content bundle's (`ingest-v1.md` §3).
pub(crate) const BUNDLE: WalkLimits = WalkLimits {
    max_entries: 4096,
    max_total: 128 * MIB,
    stored_only: true,
};

/// A game's own archive, a `.jar` or a mod `.zip` (manifest.md §2.7.1).
pub(crate) const GAME_ARCHIVE: WalkLimits = WalkLimits {
    max_entries: 65_536,
    max_total: 512 * MIB,
    stored_only: false,
};

/// A refusal with the content contract's code (`code`, its `E_…` spelling) and the reference's
/// words (`detail`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub code: &'static str,
    pub detail: String,
    /// It is a refusal of zip64 (a game archive that needs it is unscanned, never refused).
    pub zip64: bool,
    /// It is the archive's own 32 MiB of glTF JSON (a game archive over it is refused).
    pub gltf_json: bool,
}

pub(crate) fn refusal(code: &'static str, detail: impl Into<String>) -> Refusal {
    Refusal {
        code,
        detail: detail.into(),
        zip64: false,
        gltf_json: false,
    }
}

/// Why a walk stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WalkError {
    /// Refused by a rule of the contract.
    Refused(Refusal),
    /// The reference's reader raises here rather than refuse: a directory record cut short, a
    /// deflate stream that is no deflate stream.
    Unreadable(String),
}

impl From<Refusal> for WalkError {
    fn from(r: Refusal) -> Self {
        WalkError::Refused(r)
    }
}

type Walked<T> = std::result::Result<T, WalkError>;

fn refuse<T>(code: &'static str, detail: impl Into<String>) -> Walked<T> {
    Err(WalkError::Refused(refusal(code, detail)))
}

fn refuse_zip64<T>(detail: &str) -> Walked<T> {
    Err(WalkError::Refused(Refusal {
        zip64: true,
        ..refusal(E_ZIP_END_RECORD, detail)
    }))
}

/// `b[from..from + len]`, clamped to `b` as a Python slice is.
fn sl(b: &[u8], from: u64, len: u64) -> &[u8] {
    let n = b.len() as u64;
    let a = from.min(n) as usize;
    let z = from.saturating_add(len).min(n) as usize;
    &b[a..z]
}

/// `int.from_bytes(s, "little")`: whatever bytes there are.
fn le_int(s: &[u8]) -> u64 {
    s.iter()
        .rev()
        .fold(0u64, |acc, &x| (acc << 8) | u64::from(x))
}

/// `struct.unpack` of exactly `n` bytes: the reference raises on fewer.
fn exact(b: &[u8], from: u64, n: u64) -> Walked<&[u8]> {
    let s = sl(b, from, n);
    if s.len() as u64 == n {
        Ok(s)
    } else {
        Err(WalkError::Unreadable(format!(
            "a record cut short at {from}: {} of {n} bytes",
            s.len()
        )))
    }
}

fn u16_at(s: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([s[i], s[i + 1]])
}

fn u32_at(s: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]])
}

/// One entry as the directory records it.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub name: String,
    method: u16,
    crc: u32,
    csize: u32,
    /// The uncompressed size the directory records.
    pub size: u32,
    data_off: u64,
    flags: u16,
    loff: u32,
    /// The first 16 bytes of its data (fewer if it holds fewer), read at step 7.
    pub head: Vec<u8>,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

/// A walked archive: every entry in directory order (which step 3 makes the file's), each one
/// read whole once and found to be what the directory says.
pub(crate) struct Walk<'a> {
    raw: &'a [u8],
    pub entries: Vec<Entry>,
    /// The data of every file, when the walk was asked to keep it.
    kept: Option<Vec<Vec<u8>>>,
}

impl Walk<'_> {
    /// Every entry's name, folders included, in directory order: the reference's `"\0order"`.
    pub fn order(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// Every file (folders left out), its name and its data, in directory order: the reference's
    /// `files` dict.
    pub fn into_files(self) -> Vec<(String, Vec<u8>)> {
        let raw = self.raw;
        let mut kept = self.kept.map(Vec::into_iter);
        self.entries
            .into_iter()
            .filter_map(|e| {
                let data = match kept.as_mut().and_then(Iterator::next) {
                    Some(d) => d,
                    None => Walk::read_entry(raw, &e, None),
                };
                (!e.is_dir()).then_some((e.name, data))
            })
            .collect()
    }

    /// One entry's data from the archive itself, at most `at_most` bytes of it.
    fn read_entry(raw: &[u8], e: &Entry, at_most: Option<usize>) -> Vec<u8> {
        let limit = at_most.unwrap_or(usize::MAX);
        let comp = sl(raw, e.data_off, u64::from(e.csize));
        if e.method == 0 {
            return comp[..comp.len().min(limit)].to_vec();
        }
        let want = (e.size as usize).min(limit);
        let mut out = Vec::with_capacity(want);
        let _ = inflate(comp, want as u64, |piece| {
            let room = want - out.len();
            out.extend_from_slice(&piece[..piece.len().min(room)]);
        });
        out
    }

    /// Entry `i`'s data, at most `at_most` bytes of it. It was read whole at step 7, so this
    /// cannot fail.
    pub fn data(&self, i: usize, at_most: Option<usize>) -> Cow<'_, [u8]> {
        let e = &self.entries[i];
        let limit = at_most.unwrap_or(usize::MAX);
        if let Some(kept) = &self.kept {
            let d = &kept[i];
            return Cow::Borrowed(&d[..d.len().min(limit)]);
        }
        if e.method == 0 {
            let comp = sl(self.raw, e.data_off, u64::from(e.csize));
            return Cow::Borrowed(&comp[..comp.len().min(limit)]);
        }
        Cow::Owned(Walk::read_entry(self.raw, e, at_most))
    }
}

/// The step-1 end record: the LAST signature in the final 65,557 bytes must end the file
/// exactly, one disk, both counts equal, no zip64, and its directory where it says.
fn end_record(b: &[u8]) -> Walked<(u64, u64, u64)> {
    let start = b.len().saturating_sub(22 + 65535);
    let tail = &b[start..];
    let Some(at) = memchr::memmem::rfind(tail, END).filter(|at| at + 22 <= tail.len()) else {
        return refuse(E_ZIP_END_RECORD, "no end record at the end");
    };
    let eocd = start + at;
    let r = &b[eocd..eocd + 22];
    let (disk, disk_cd, on_disk, total) = (u16_at(r, 4), u16_at(r, 6), u16_at(r, 8), u16_at(r, 10));
    let (cd_size, cd_off, clen) = (u32_at(r, 12), u32_at(r, 16), u16_at(r, 20));
    if eocd + 22 + usize::from(clen) != b.len() {
        return refuse(
            E_ZIP_END_RECORD,
            "the last end record does not end the file",
        );
    }
    if disk != 0 || disk_cd != 0 || on_disk != total {
        return refuse(E_ZIP_END_RECORD, "several disks");
    }
    if total == 0xFFFF || cd_off == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF {
        return refuse_zip64("zip64 is not accepted in v1");
    }
    let (cd_off, cd_size) = (u64::from(cd_off), u64::from(cd_size));
    if total != 0 && (cd_off + cd_size != eocd as u64 || sl(b, cd_off, 4) != CEN) {
        return refuse(
            E_ZIP_END_RECORD,
            "the directory is not where the record says",
        );
    }
    Ok((u64::from(total), cd_off, cd_size))
}

/// The local header's extra field at `loff`, as the reference slices it (nothing where it is not).
fn local_extra(b: &[u8], loff: u64) -> &[u8] {
    let name_len = le_int(sl(b, loff + 26, 2));
    let extra_len = le_int(sl(b, loff + 28, 2));
    sl(b, loff + 30 + name_len, extra_len)
}

/// The ids of the extra fields of `blob`.
fn extra_ids(blob: &[u8]) -> Vec<u16> {
    let mut ids = Vec::new();
    let mut q = 0usize;
    while q + 4 <= blob.len() {
        ids.push(u16_at(blob, q));
        q += 4 + usize::from(u16_at(blob, q + 2));
    }
    ids
}

/// Step 3's first pass, over ALL directory entries before any other step-3 check: zip64 in any
/// spelling — a sentinel size or offset, or a `0x0001` extra field, central or local.
fn zip64_prepass(b: &[u8], total: u64, cd_off: u64) -> Walked<()> {
    let mut p = cd_off;
    for _ in 0..total {
        if sl(b, p, 4) != CEN {
            return Ok(()); // not a directory entry: the main pass says so, with its own code
        }
        let sizes = exact(b, p + 20, 8)?;
        let (csize, usize_) = (u32_at(sizes, 0), u32_at(sizes, 4));
        let lens = exact(b, p + 28, 6)?;
        let (nlen, xlen, clen) = (
            u64::from(u16_at(lens, 0)),
            u64::from(u16_at(lens, 2)),
            u64::from(u16_at(lens, 4)),
        );
        let loff = u32_at(exact(b, p + 42, 4)?, 0);
        let extra = sl(b, p + 46 + nlen, xlen);
        p += 46 + nlen + xlen + clen;
        if [csize, usize_, loff].contains(&0xFFFF_FFFF) {
            return refuse_zip64("a zip64 sentinel size or offset");
        }
        for blob in [extra, local_extra(b, u64::from(loff))] {
            if extra_ids(blob).contains(&0x0001) {
                return refuse_zip64("a zip64 extra field");
            }
        }
    }
    Ok(())
}

/// A non-DOS host with a unix mode in the high 16 bits that is not a regular file or a folder.
fn is_link(made: u16, ext: u32) -> bool {
    if made >> 8 == 0 || ext >> 16 == 0 {
        return false;
    }
    !matches!((ext >> 16) & 0o170000, 0o100000 | 0o040000)
}

/// The part-digest path rules (`part-digest-v1.md` §2, `contentlite.path_refusal`).
pub(crate) fn path_refusal(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("empty");
    }
    if !unicode_normalization::is_nfc(path) {
        return Some("not-nfc");
    }
    if path.contains('\\') {
        return Some("backslash");
    }
    if path.starts_with('/') {
        return Some("leading-slash");
    }
    if path
        .chars()
        .any(|c| (c as u32) <= 0x1F || (0x7F..=0x9F).contains(&(c as u32)))
    {
        return Some("control-character");
    }
    for seg in path.split('/') {
        if seg.is_empty() {
            return Some("empty-segment");
        }
        if seg == ".." {
            return Some("dot-dot");
        }
    }
    None
}

/// The reference's `DEVICE` regular expression, `re.I`: a Windows device name, with or without an
/// extension. Python folds `İ` and `ı` to `i` when it ignores case, so `conın$` is one.
fn is_device(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment);
    let folded: String = stem
        .chars()
        .map(|c| match c {
            '\u{0130}' | '\u{0131}' => 'i',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    let numbered = |prefix: &str| {
        folded.strip_prefix(prefix).is_some_and(|n| {
            let mut c = n.chars();
            matches!(
                (c.next(), c.next()),
                (Some('0'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}'), None)
            )
        })
    };
    matches!(
        folded.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) || numbered("com")
        || numbered("lpt")
}

/// Step 4's rule for one name, or why it breaks it.
pub(crate) fn name_refusal(name: &str) -> Option<String> {
    let body = name.trim_end_matches('/');
    let checked = if name.ends_with('/') { body } else { name };
    if let Some(why) = path_refusal(checked) {
        return Some(why.to_owned());
    }
    if name.len() > MAX_NAME {
        return Some("too long".to_owned());
    }
    let segs: Vec<&str> = body.split('/').collect();
    if segs.len() > MAX_SEGMENTS {
        return Some("too deep".to_owned());
    }
    for s in segs {
        if s == "." || s.contains(':') || is_device(s) || s.trim() != s || s.ends_with('.') {
            return Some(format!("segment {s:?}"));
        }
    }
    None
}

/// The key two names collide under on a case-insensitive file system: NFC, then upper and lower
/// case with the full Unicode mappings, twice (the contract's N8).
pub(crate) fn fold_key(name: &str) -> String {
    let nfc: String = name.nfc().collect();
    nfc.to_uppercase()
        .to_lowercase()
        .to_uppercase()
        .to_lowercase()
}

/// The glTF JSON a reader would hold for an entry: a GLB's declared first-chunk length (from its
/// first 16 bytes), or a `.gltf`'s whole size — before anything is parsed.
pub(crate) fn gltf_json_bytes(path: &str, head: &[u8], size: u64) -> u64 {
    if head.starts_with(GLB_MAGIC) {
        return if head.len() >= 16 {
            u64::from(u32_at(head, 12))
        } else {
            0
        };
    }
    if path.to_lowercase().ends_with(".gltf") {
        size
    } else {
        0
    }
}

/// What a raw deflate stream did, read to its end or to `cap` bytes out.
struct Inflated {
    /// The stream ended.
    eof: bool,
    /// Input bytes it consumed.
    consumed: u64,
    produced: u64,
}

/// Inflates `comp` (raw deflate), at most `cap` bytes out, handing each piece of output to
/// `sink`. `Err` is a stream that is no deflate stream.
fn inflate(comp: &[u8], cap: u64, mut sink: impl FnMut(&[u8])) -> Result<Inflated, String> {
    let mut d = Decompress::new(false);
    let mut out = vec![0u8; 64 * 1024];
    loop {
        let room = (cap - d.total_out()).min(out.len() as u64) as usize;
        if room == 0 {
            break;
        }
        let (was_in, was_out) = (d.total_in(), d.total_out());
        let status = d
            .decompress(
                &comp[was_in as usize..],
                &mut out[..room],
                FlushDecompress::None,
            )
            .map_err(|e| e.to_string())?;
        let produced = (d.total_out() - was_out) as usize;
        sink(&out[..produced]);
        if status == Status::StreamEnd {
            return Ok(Inflated {
                eof: true,
                consumed: d.total_in(),
                produced: d.total_out(),
            });
        }
        if d.total_in() == was_in && produced == 0 {
            break; // every byte is in and the stream has not ended
        }
    }
    Ok(Inflated {
        eof: false,
        consumed: d.total_in(),
        produced: d.total_out(),
    })
}

/// The first `n` bytes of an entry's data, inflating no more than that (nothing, if it does not
/// inflate).
fn peek(b: &[u8], method: u16, off: u64, csize: u32, n: usize) -> Vec<u8> {
    let comp = sl(b, off, u64::from(csize));
    if method == 0 {
        return comp[..comp.len().min(n)].to_vec();
    }
    let mut out = Vec::with_capacity(n);
    match inflate(comp, n as u64, |piece| out.extend_from_slice(piece)) {
        Ok(_) => out,
        Err(_) => Vec::new(),
    }
}

/// Walks `b` under `limits` (the contract's steps 1–7). With `keep`, every file's data is kept
/// for [`Walk::data`]; without, it is read again from `b` when asked.
pub(crate) fn walk(b: &[u8], limits: WalkLimits, keep: bool) -> Walked<Walk<'_>> {
    let (total, cd_off, cd_size) = end_record(b)?;
    if total > limits.max_entries {
        return refuse(E_ARCHIVE_TOO_MANY_ENTRIES, total.to_string());
    }
    zip64_prepass(b, total, cd_off)?;
    let mut entries: Vec<Entry> = Vec::with_capacity(total as usize);
    let mut p = cd_off;
    for _ in 0..total {
        if sl(b, p, 4) != CEN {
            return refuse(
                E_ZIP_HEADERS_INCONSISTENT,
                "a directory entry that is not one",
            );
        }
        let h = exact(b, p, 46)?;
        let (made, flags, method) = (u16_at(h, 4), u16_at(h, 8), u16_at(h, 10));
        let (crc, csize, usize_) = (u32_at(h, 16), u32_at(h, 20), u32_at(h, 24));
        let (nlen, xlen, clen) = (
            u64::from(u16_at(h, 28)),
            u64::from(u16_at(h, 30)),
            u64::from(u16_at(h, 32)),
        );
        let (ext, loff) = (u32_at(h, 38), u32_at(h, 42));
        let raw_name = sl(b, p + 46, nlen);
        let extra = sl(b, p + 46 + nlen, xlen);
        p += 46 + nlen + xlen + clen;
        let lx = local_extra(b, u64::from(loff));
        // The Info-ZIP Unicode Path field, in a central OR a local header.
        let upath = [extra, lx]
            .into_iter()
            .any(|blob| extra_ids(blob).contains(&0x7075));
        if flags & 1 != 0 {
            return refuse(E_ARCHIVE_ENCRYPTED, String::from_utf8_lossy(raw_name));
        }
        if method != 0 && method != 8 {
            return refuse(E_ARCHIVE_METHOD, method.to_string());
        }
        if is_link(made, ext) {
            return refuse(E_ARCHIVE_LINK, String::from_utf8_lossy(raw_name));
        }
        if upath {
            return refuse(E_ZIP_HEADERS_INCONSISTENT, "a Unicode Path extra field");
        }
        let Ok(name) = std::str::from_utf8(raw_name) else {
            return refuse(E_ARCHIVE_NAME_UNSAFE, "not UTF-8");
        };
        let name = name.to_owned();
        let lh = sl(b, u64::from(loff), 30);
        if lh.len() < 30 || &lh[..4] != LOC {
            return refuse(
                E_ZIP_HEADERS_INCONSISTENT,
                format!("{name}: no local header"),
            );
        }
        let (lflags, lmethod) = (u16_at(lh, 6), u16_at(lh, 8));
        let (lcrc, lcs, lus) = (u32_at(lh, 14), u32_at(lh, 18), u32_at(lh, 22));
        let (lnl, lxl) = (u64::from(u16_at(lh, 26)), u64::from(u16_at(lh, 28)));
        let lname = sl(b, u64::from(loff) + 30, lnl);
        if lname != raw_name || lmethod != method || (lflags & 0x9) != (flags & 0x9) {
            return refuse(
                E_ZIP_HEADERS_INCONSISTENT,
                format!("{name}: local and central headers disagree"),
            );
        }
        if flags & 0x8 != 0 && method == 0 {
            return refuse(
                E_ZIP_HEADERS_INCONSISTENT,
                format!("{name}: bit 3 on a Stored entry"),
            );
        }
        if (lcrc, lcs, lus) != (crc, csize, usize_)
            && !(flags & 0x8 != 0 && (lcrc, lcs, lus) == (0, 0, 0))
        {
            return refuse(
                E_ZIP_HEADERS_INCONSISTENT,
                format!("{name}: local and central sizes disagree"),
            );
        }
        if name.ends_with('/') && (csize != 0 || usize_ != 0) {
            return refuse(
                E_ZIP_HEADERS_INCONSISTENT,
                format!("{name}: a directory entry that carries data"),
            );
        }
        entries.push(Entry {
            name,
            method,
            crc,
            csize,
            size: usize_,
            data_off: u64::from(loff) + 30 + lnl + lxl,
            flags,
            loff,
            head: Vec::new(),
        });
    }
    // The count must account for the WHOLE directory.
    if p != cd_off + cd_size {
        return refuse(
            E_ZIP_HEADERS_INCONSISTENT,
            "the directory holds more than the end record counts",
        );
    }
    // The entries TILE the file: the first local header at 0, each entry's data (and descriptor)
    // followed at once by the next header, the last one ending where the directory starts.
    let mut crc_of: HashMap<&str, (u32, u32)> = HashMap::new();
    for e in &entries {
        crc_of.insert(e.name.as_str(), (e.crc, e.size)); // the last of a name, as a dict keeps it
    }
    let mut by_offset: Vec<&Entry> = entries.iter().collect();
    by_offset.sort_by_key(|e| e.loff);
    let mut at: u64 = 0;
    for e in by_offset {
        let loff = u64::from(e.loff);
        if loff != at {
            let what = if loff > at { "a gap" } else { "an overlap" };
            return refuse(
                E_ZIP_HEADERS_INCONSISTENT,
                format!("{}: {what} before it", e.name),
            );
        }
        at = e.data_off + u64::from(e.csize);
        if e.flags & 0x8 != 0 {
            let (crc, size) = crc_of[e.name.as_str()];
            let mut want = Vec::with_capacity(12);
            for v in [crc, e.csize, size] {
                want.extend_from_slice(&v.to_le_bytes());
            }
            let mut signed = DESCRIPTOR.to_vec();
            signed.extend_from_slice(&want);
            if sl(b, at, 16) == signed.as_slice() {
                at += 16;
            } else if sl(b, at, 12) == want.as_slice() {
                at += 12;
            } else {
                return refuse(
                    E_ZIP_HEADERS_INCONSISTENT,
                    format!(
                        "{}: the data descriptor disagrees with the directory",
                        e.name
                    ),
                );
            }
        }
    }
    if at != cd_off {
        return refuse(
            E_ZIP_HEADERS_INCONSISTENT,
            "bytes between the last entry and the directory",
        );
    }
    if entries.windows(2).any(|w| w[0].loff > w[1].loff) {
        return refuse(
            E_ZIP_HEADERS_INCONSISTENT,
            "the directory's order is not the file's order",
        );
    }
    // Step 4: the names.
    let mut folded: HashSet<String> = HashSet::new();
    for e in &entries {
        if let Some(why) = name_refusal(&e.name) {
            return refuse(E_ARCHIVE_NAME_UNSAFE, format!("{:?}: {why}", e.name));
        }
        if !folded.insert(fold_key(&e.name)) {
            return refuse(
                E_ARCHIVE_NAME_UNSAFE,
                format!("{:?}: collides under case folding", e.name),
            );
        }
    }
    let mut dirs_of: HashSet<String> = HashSet::new();
    for e in &entries {
        let segs = e.name.trim_end_matches('/').split('/').count();
        let key = fold_key(&e.name);
        let key_segs: Vec<&str> = key.trim_end_matches('/').split('/').collect();
        for i in 1..segs {
            dirs_of.insert(key_segs[..i.min(key_segs.len())].join("/"));
        }
    }
    for e in &entries {
        if !e.is_dir() && dirs_of.contains(&fold_key(&e.name)) {
            return refuse(
                E_ARCHIVE_NAME_UNSAFE,
                format!("{:?}: a file and a directory at one path", e.name),
            );
        }
    }
    // Step 5: the sizes, then the archive's glTF JSON, from headers and first bytes.
    if entries.iter().map(|e| u64::from(e.size)).sum::<u64>() > limits.max_total {
        return refuse(E_ARCHIVE_TOO_LARGE, "uncompressed total");
    }
    for e in &entries {
        if e.method == 8 && u64::from(e.size) > MAX_RATIO * u64::from(e.csize.max(1)) {
            return refuse(E_ARCHIVE_RATIO, e.name.clone());
        }
    }
    let gltf_total: u64 = entries
        .iter()
        .filter(|e| !e.is_dir())
        .map(|e| {
            let head = peek(b, e.method, e.data_off, e.csize, 16);
            gltf_json_bytes(&e.name, &head, u64::from(e.size))
        })
        .sum();
    if gltf_total > MAX_GLTF_JSON_TOTAL {
        return Err(WalkError::Refused(Refusal {
            gltf_json: true,
            ..refusal(
                E_ARCHIVE_TOO_LARGE,
                format!("{gltf_total} bytes of glTF JSON, over {MAX_GLTF_JSON_TOTAL}"),
            )
        }));
    }
    // Step 6.
    if limits.stored_only {
        for e in &entries {
            let lower = e.name.to_lowercase();
            if e.method != 0 && STORED_ONLY.iter().any(|x| lower.ends_with(x)) {
                return refuse(E_ARCHIVE_MUST_BE_STORED, e.name.clone());
            }
        }
    }
    // Step 7: the raw read. A deflate stream ends exactly at its compressed size, inflating to
    // exactly the recorded size and CRC.
    let mut kept: Option<Vec<Vec<u8>>> = keep.then(Vec::new);
    for e in &mut entries {
        let comp = sl(b, e.data_off, u64::from(e.csize));
        let mut crc = crc32fast::Hasher::new();
        let mut head: Vec<u8> = Vec::with_capacity(16);
        let mut data: Vec<u8> = Vec::new();
        let size = e.size as usize;
        let mut take = |piece: &[u8]| {
            crc.update(piece);
            if head.len() < 16 {
                head.extend_from_slice(&piece[..piece.len().min(16 - head.len())]);
            }
            if keep && data.len() <= size {
                data.extend_from_slice(&piece[..piece.len().min(size + 1 - data.len())]);
            }
        };
        let produced = if e.is_dir() {
            0
        } else if e.method == 0 {
            take(comp);
            comp.len() as u64
        } else {
            let got = inflate(comp, limits.max_total + 1, &mut take)
                .map_err(|why| WalkError::Unreadable(format!("{}: {why}", e.name)))?;
            if !got.eof || got.consumed != comp.len() as u64 {
                return refuse(
                    E_ZIP_HEADERS_INCONSISTENT,
                    format!("{}: the deflate stream does not end at its size", e.name),
                );
            }
            got.produced
        };
        if !e.is_dir() && (produced != u64::from(e.size) || crc.finalize() != e.crc) {
            return refuse(
                E_ZIP_HEADERS_INCONSISTENT,
                format!("{}: size or CRC", e.name),
            );
        }
        e.head = head;
        if let Some(k) = &mut kept {
            k.push(data);
        }
    }
    Ok(Walk {
        raw: b,
        entries,
        kept,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_refused_as_the_contract_reads_them() {
        for (name, refused) in [
            ("a/b.png", false),
            ("a/", false),
            ("con.txt", true),
            ("x/CONIN$", true),
            ("x/con\u{0131}n$.log", true),
            ("x/com\u{00B9}", true),
            ("x/com10", false),
            ("a/./b", true),
            ("a b", false),
            (" a", true),
            ("a.", true),
            ("a:b", true),
            ("a//b", true),
            ("../a", true),
            ("e\u{0301}.png", true), // not NFC
            ("\u{00E9}.png", false),
        ] {
            assert_eq!(name_refusal(name).is_some(), refused, "{name:?}");
        }
        assert!(name_refusal(&"a/".repeat(17)).is_some(), "too deep");
        assert!(name_refusal(&"a".repeat(241)).is_some(), "too long");
    }

    #[test]
    fn names_fold_twice() {
        assert_eq!(fold_key("Stra\u{00DF}e"), fold_key("STRASSE"));
        assert_eq!(fold_key("\u{1E9E}"), fold_key("ss"));
        assert_eq!(fold_key("\u{017F}"), fold_key("s"));
        assert_eq!(fold_key("\u{03C2}"), fold_key("\u{03C3}"));
    }

    #[test]
    fn a_python_slice_is_clamped_and_its_int_reads_what_is_there() {
        assert_eq!(sl(b"abc", 2, 5), b"c");
        assert_eq!(sl(b"abc", 9, 5), b"");
        assert_eq!(le_int(&[0x34, 0x12]), 0x1234);
        assert_eq!(le_int(&[]), 0);
        assert!(matches!(exact(b"abc", 1, 4), Err(WalkError::Unreadable(_))));
    }
}
