// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A loader's version (engine-descriptor-v1.md §7.4): the strings of one PE file's `RT_VERSION`
//! resource, reached through its headers, its section table and its resource directory — a
//! bounded probe, never a scan of the file. Every read is counted against a budget (at most
//! [`super::limits::VERSION_BYTES`]): a read the budget would not cover is not made, and the
//! probe says so (`Counted::exhausted`; the evaluation reads that as over its bound). A file
//! that is no PE, or holds no such resource, has no version.
//!
//! `VS_VERSIONINFO` and every block below it (`StringFileInfo`, a `StringTable`, a `String`) is
//! `u16 wLength, u16 wValueLength, u16 wType, WCHAR szKey[]` (NUL-terminated), padding to a
//! 32-bit boundary, the value, padding, then its children. A `String`'s value is read up to its
//! NUL or its block's end, whichever is first, whatever its `wValueLength` counts (writers
//! disagree: characters or bytes).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use super::VersionKey;

/// `RT_VERSION`.
const RT_VERSION: u32 = 16;
/// Sections and resource-directory entries read at most (a real DLL has a handful of each).
const MAX_SECTIONS: usize = 96;
const MAX_DIR_ENTRIES: usize = 128;

/// Reads at offsets of one open file, every byte counted against `budget`.
pub(crate) struct Counted {
    f: File,
    len: u64,
    budget: u64,
    pub(crate) used: u64,
    /// A read was refused because the budget would not cover it.
    pub(crate) exhausted: bool,
}

impl Counted {
    pub(crate) fn new(f: File, len: u64, budget: u64) -> Counted {
        Counted {
            f,
            len,
            budget,
            used: 0,
            exhausted: false,
        }
    }

    fn read(&mut self, off: u64, n: usize) -> Option<Vec<u8>> {
        let n64 = n as u64;
        if off.checked_add(n64)? > self.len {
            return None;
        }
        if n64 > self.budget - self.used {
            self.exhausted = true;
            return None;
        }
        self.used += n64;
        self.f.seek(SeekFrom::Start(off)).ok()?;
        let mut b = vec![0u8; n];
        self.f.read_exact(&mut b).ok()?;
        Some(b)
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// One section's `(virtual address, virtual size, raw size, raw pointer)`.
type Section = (u32, u32, u32, u32);

/// Where `n` bytes at `rva` sit in the file, when one section holds all of them on disk.
fn file_offset(sections: &[Section], rva: u32, n: u32) -> Option<u64> {
    sections.iter().find_map(|&(va, vsize, raw_size, raw_ptr)| {
        let into = rva.checked_sub(va)?;
        let end = into.checked_add(n)?;
        (into < vsize.max(raw_size) && end <= raw_size)
            .then(|| u64::from(raw_ptr) + u64::from(into))
    })
}

/// The value of the first `String` entry of each of `keys` in the file's `StringFileInfo`, in
/// `keys`' order (`None` where it has none); `None` for a file that is no PE or has no version
/// resource within the budget.
pub(crate) fn version_strings(r: &mut Counted, keys: &[VersionKey]) -> Option<Vec<Option<String>>> {
    let data = version_resource(r)?;
    Some(
        keys.iter()
            .map(|k| string_value(&data, k.as_str()))
            .collect(),
    )
}

fn version_resource(r: &mut Counted) -> Option<Vec<u8>> {
    let mz = r.read(0, 64)?;
    if &mz[..2] != b"MZ" {
        return None;
    }
    let pe_at = u64::from(u32_at(&mz, 0x3C)?);
    let coff = r.read(pe_at, 24)?;
    if &coff[..4] != b"PE\0\0" {
        return None;
    }
    let sections = usize::from(u16_at(&coff, 6)?);
    let opt_size = usize::from(u16_at(&coff, 20)?);
    if sections == 0 || sections > MAX_SECTIONS || !(2..=1024).contains(&opt_size) {
        return None;
    }
    let opt = r.read(pe_at + 24, opt_size)?;
    let (count_at, dirs_at) = match u16_at(&opt, 0)? {
        0x10B => (92, 96),
        0x20B => (108, 112),
        _ => return None,
    };
    if u32_at(&opt, count_at)? < 3 {
        return None;
    }
    let res_rva = u32_at(&opt, dirs_at + 16)?;
    if res_rva == 0 {
        return None;
    }
    let table = r.read(pe_at + 24 + opt_size as u64, sections * 40)?;
    let sections: Vec<Section> = table
        .as_chunks::<40>()
        .0
        .iter()
        .map(|s| {
            Some((
                u32_at(s, 12)?,
                u32_at(s, 8)?,
                u32_at(s, 16)?,
                u32_at(s, 20)?,
            ))
        })
        .collect::<Option<_>>()?;
    // Type → name → language → data entry.
    let mut dir = 0u32;
    for level in 0..3 {
        let head = r.read(file_offset(&sections, res_rva.checked_add(dir)?, 16)?, 16)?;
        let n = usize::from(u16_at(&head, 12)?) + usize::from(u16_at(&head, 14)?);
        if n == 0 || n > MAX_DIR_ENTRIES {
            return None;
        }
        let at = res_rva.checked_add(dir)?.checked_add(16)?;
        let entries = r.read(file_offset(&sections, at, (n * 8) as u32)?, n * 8)?;
        let pick = entries.as_chunks::<8>().0.iter().find(|e| {
            level != 0
                || u32_at(e.as_slice(), 0)
                    .is_some_and(|name| name & 0x8000_0000 == 0 && name == RT_VERSION)
        })?;
        let off = u32_at(pick.as_slice(), 4)?;
        let sub = off & 0x8000_0000 != 0;
        if sub != (level < 2) {
            return None;
        }
        dir = off & 0x7FFF_FFFF;
    }
    let entry = r.read(file_offset(&sections, res_rva.checked_add(dir)?, 16)?, 16)?;
    let (data_rva, size) = (u32_at(&entry, 0)?, u32_at(&entry, 4)?);
    r.read(file_offset(&sections, data_rva, size)?, size as usize)
}

/// One block of a `VS_VERSIONINFO` tree, at `start` of `b`.
struct Block<'b> {
    key: String,
    value: &'b [u8],
    text: bool,
    value_units: usize,
    children: usize,
    end: usize,
}

fn align4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

fn block(b: &[u8], start: usize) -> Option<Block<'_>> {
    let len = usize::from(u16_at(b, start)?);
    let end = start.checked_add(len)?;
    if len < 6 || end > b.len() {
        return None;
    }
    let value_len = usize::from(u16_at(b, start + 2)?);
    let text = u16_at(b, start + 4)? == 1;
    let mut units = Vec::new();
    let mut at = start + 6;
    loop {
        let u = u16_at(&b[..end], at)?;
        at += 2;
        if u == 0 {
            break;
        }
        units.push(u);
    }
    let value_at = align4(at).min(end);
    let value_bytes = if text { value_len * 2 } else { value_len };
    let children = align4(value_at.saturating_add(value_bytes)).min(end);
    Some(Block {
        key: String::from_utf16(&units).ok()?,
        value: &b[value_at..end],
        text,
        value_units: value_len,
        children,
        end,
    })
}

/// The blocks directly below `parent`.
fn children<'b>(b: &'b [u8], parent: &Block<'_>) -> Vec<Block<'b>> {
    let mut out = Vec::new();
    let mut at = parent.children;
    while at + 6 <= parent.end {
        let Some(c) = block(b, at).filter(|c| c.end <= parent.end) else {
            break;
        };
        let next = align4(c.end);
        out.push(c);
        if next <= at {
            break;
        }
        at = next;
    }
    out
}

/// The value of the first `String` named `key` in `data`'s `StringFileInfo`, trimmed; `None`
/// when there is none, or it is empty.
fn string_value(data: &[u8], key: &str) -> Option<String> {
    let root = block(data, 0)?;
    if root.key != "VS_VERSION_INFO" {
        return None;
    }
    for info in children(data, &root) {
        if info.key != "StringFileInfo" {
            continue;
        }
        for table in children(data, &info) {
            for s in children(data, &table) {
                if s.key != key || !s.text {
                    continue;
                }
                let units: Vec<u16> = s
                    .value
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .take(s.value_units)
                    .take_while(|u| *u != 0)
                    .collect();
                let v = String::from_utf16(&units).ok()?;
                let v = v.trim();
                return (!v.is_empty()).then(|| v.to_owned());
            }
        }
    }
    None
}

/// Test builders: a PE with a version resource holding the given strings.
#[cfg(test)]
pub(crate) mod build {
    fn utf16z(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    fn pad4(v: &mut Vec<u8>) {
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
    }

    /// One block: its header, key, value and children, `wLength` filled in.
    fn block(key: &str, value: &[u8], value_len: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
        let mut b = vec![0, 0];
        b.extend(value_len.to_le_bytes());
        b.extend(u16::from(text).to_le_bytes());
        b.extend(utf16z(key));
        pad4(&mut b);
        b.extend(value);
        for c in children {
            pad4(&mut b);
            b.extend(c);
        }
        let len = b.len() as u16;
        b[..2].copy_from_slice(&len.to_le_bytes());
        b
    }

    /// `VS_VERSIONINFO` holding one `StringTable` of `strings`.
    pub(crate) fn version_info(strings: &[(&str, &str)]) -> Vec<u8> {
        let entries: Vec<Vec<u8>> = strings
            .iter()
            .map(|(k, v)| {
                let value = utf16z(v);
                block(k, &value, (value.len() / 2) as u16, true, &[])
            })
            .collect();
        let table = block("040904b0", &[], 0, true, &entries);
        let info = block("StringFileInfo", &[], 0, true, &[table]);
        let mut fixed = vec![0u8; 52];
        fixed[..4].copy_from_slice(&0xFEEF_04BDu32.to_le_bytes());
        block("VS_VERSION_INFO", &fixed, 52, false, &[info])
    }

    /// A PE32+ image whose one section `.rsrc` holds a resource directory with an `RT_VERSION`
    /// of `strings`, and `pad` zero bytes before the section (to push it past a budget).
    pub(crate) fn pe(strings: &[(&str, &str)], pad: usize) -> Vec<u8> {
        let info = version_info(strings);
        let section_raw = 0x400 + pad as u32;
        let va = 0x1000u32;
        // Resource tree, offsets within .rsrc: the type directory at 0 (an RT_ICON entry, to an
        // offset never followed, sorted before RT_VERSION), the name directory at 32, the
        // language directory at 56, the data entry at 80, the data at 96.
        let dir = |entries: &[(u32, u32)]| {
            let mut d = vec![0u8; 12];
            d.extend(0u16.to_le_bytes());
            d.extend((entries.len() as u16).to_le_bytes());
            for (name, off) in entries {
                d.extend(name.to_le_bytes());
                d.extend(off.to_le_bytes());
            }
            d
        };
        let mut rsrc = dir(&[(3, 0x8000_0200), (16, 0x8000_0000 | 32)]);
        rsrc.extend(dir(&[(1, 0x8000_0000 | 56)]));
        rsrc.extend(dir(&[(0x409, 80)]));
        let data_at = 96u32;
        rsrc.extend((va + data_at).to_le_bytes());
        rsrc.extend((info.len() as u32).to_le_bytes());
        rsrc.extend([0u8; 8]);
        assert_eq!(rsrc.len(), data_at as usize);
        rsrc.extend(&info);
        let raw_size = rsrc.len().div_ceil(0x200) * 0x200;
        rsrc.resize(raw_size, 0);

        let mut b = vec![0u8; section_raw as usize];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        let pe = 0x80;
        b[pe..pe + 4].copy_from_slice(b"PE\0\0");
        b[pe + 4..pe + 6].copy_from_slice(&0x8664u16.to_le_bytes());
        b[pe + 6..pe + 8].copy_from_slice(&1u16.to_le_bytes());
        let opt_size = 240u16;
        b[pe + 20..pe + 22].copy_from_slice(&opt_size.to_le_bytes());
        let opt = pe + 24;
        b[opt..opt + 2].copy_from_slice(&0x20Bu16.to_le_bytes());
        b[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        b[opt + 112 + 16..opt + 112 + 20].copy_from_slice(&va.to_le_bytes());
        b[opt + 112 + 20..opt + 112 + 24].copy_from_slice(&(raw_size as u32).to_le_bytes());
        let sec = opt + usize::from(opt_size);
        b[sec..sec + 5].copy_from_slice(b".rsrc");
        b[sec + 8..sec + 12].copy_from_slice(&(raw_size as u32).to_le_bytes());
        b[sec + 12..sec + 16].copy_from_slice(&va.to_le_bytes());
        b[sec + 16..sec + 20].copy_from_slice(&(raw_size as u32).to_le_bytes());
        b[sec + 20..sec + 24].copy_from_slice(&section_raw.to_le_bytes());
        b.extend(rsrc);
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(
        bytes: &[u8],
        keys: &[VersionKey],
        budget: u64,
    ) -> (Option<Vec<Option<String>>>, Counted) {
        // One folder per call: tests run in parallel, and two files may be the same length.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("agl-pe-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.dll");
        std::fs::write(&p, bytes).unwrap();
        let (f, len) = crate::engines::reads::open_regular(&p).unwrap();
        let mut r = Counted::new(f, len, budget);
        let v = version_strings(&mut r, keys);
        let _ = std::fs::remove_dir_all(&dir);
        (v, r)
    }

    use VersionKey::{FileVersion, ProductVersion};

    #[test]
    fn the_version_strings_are_read_through_the_resource_directory() {
        let pe = build::pe(
            &[
                ("FileVersion", "5.4.23.3"),
                ("ProductVersion", "5.4.23.3+b89fcc8"),
            ],
            0,
        );
        let (v, r) = probe(&pe, &[ProductVersion, FileVersion], 32 * 1024);
        assert_eq!(
            v,
            Some(vec![
                Some("5.4.23.3+b89fcc8".to_owned()),
                Some("5.4.23.3".to_owned())
            ])
        );
        assert!(r.used < 2048, "{} bytes read", r.used);
        // The same as the byte scan today's discovery reads it with.
        assert_eq!(
            crate::mods::version::version_string(&pe, "ProductVersion").as_deref(),
            Some("5.4.23.3+b89fcc8")
        );
        let (v, _) = probe(&pe, &[FileVersion], 32 * 1024);
        assert_eq!(v, Some(vec![Some("5.4.23.3".to_owned())]));
    }

    #[test]
    fn a_resource_past_the_budget_is_no_version() {
        // The resource itself sits 40 KiB in; the reads that reach it stay small, so a far
        // resource is still read: the bound counts bytes READ, not where they are.
        let far = build::pe(&[("ProductVersion", "3.0.1")], 40 * 1024);
        let (v, r) = probe(&far, &[ProductVersion], 32 * 1024);
        assert_eq!(v, Some(vec![Some("3.0.1".to_owned())]));
        assert!(!r.exhausted);
        // A budget too small to read the headers: nothing, and said so.
        let (v, r) = probe(&far, &[ProductVersion], 200);
        assert_eq!(v, None);
        assert!(r.exhausted);
        assert!(r.used <= 200);
    }

    #[test]
    fn what_is_no_pe_or_has_no_entry_has_no_version() {
        let (v, _) = probe(
            b"MZ not a pe at all, just bytes and more bytes and more bytes .....",
            &[ProductVersion],
            32 * 1024,
        );
        assert_eq!(v, None);
        let pe = build::pe(&[("CompanyName", "x")], 0);
        let (v, _) = probe(&pe, &[ProductVersion], 32 * 1024);
        assert_eq!(v, Some(vec![None]));
        let mut broken = build::pe(&[("ProductVersion", "1.2.3")], 0);
        // Cut it short inside the resource: never a panic, no version.
        broken.truncate(broken.len() - 0x180);
        let (v, _) = probe(&broken, &[ProductVersion], 32 * 1024);
        assert_eq!(v, None);
    }
}
