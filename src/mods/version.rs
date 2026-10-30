// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! One string of a PE file's version resource (`ProductVersion`, `ProductName`, …), read with a
//! bounded byte scan in the style of `doorstop::understands`: no resource-directory walk, because
//! the question is only what one `String` entry of the `StringFileInfo` says.
//!
//! A `String` entry is `u16 wLength, u16 wValueLength, u16 wType (1 = text), WCHAR szKey[]`
//! (NUL-terminated), padding to a 32-bit boundary, then `WCHAR Value[wValueLength]`. The key is
//! found as UTF-16LE with its NUL, the header in front of it must say "text", and the value must
//! end inside the entry: anything else is not a version string and is skipped.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// More than any assembly or proxy whose version is read here (ReShade is ~6 MB).
pub(crate) const MAX_VERSIONED_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// More than any version string.
const MAX_VALUE_CHARS: usize = 512;

/// The value of `key` in the first `String` entry of `bin`'s version resource that names it,
/// trimmed; `None` when there is none, or it is empty.
pub(crate) fn version_string(bin: &[u8], key: &str) -> Option<String> {
    let mut needle: Vec<u8> = key.encode_utf16().flat_map(u16::to_le_bytes).collect();
    needle.extend([0, 0]);
    let mut from = 0;
    while let Some(found) = find(&bin[from..], &needle) {
        let at = from + found;
        from = at + 1;
        if let Some(v) = entry_value(bin, at, needle.len()) {
            return Some(v);
        }
    }
    None
}

/// The value of the `String` entry whose key (with its NUL, `key_len` bytes) starts at `at`.
fn entry_value(bin: &[u8], at: usize, key_len: usize) -> Option<String> {
    let start = at.checked_sub(6)?;
    let u16_at = |o: usize| -> Option<usize> {
        let b = bin.get(o..o + 2)?;
        Some(u16::from_le_bytes([b[0], b[1]]) as usize)
    };
    let length = u16_at(start)?;
    let chars = u16_at(start + 2)?;
    if u16_at(start + 4)? != 1 || chars == 0 || chars > MAX_VALUE_CHARS {
        return None;
    }
    // The value starts at the next 32-bit boundary after the key, counted from the entry.
    let value = start + (6 + key_len).div_ceil(4) * 4;
    let end = value.checked_add(chars * 2)?;
    if end > start + length || end > bin.len() {
        return None;
    }
    let units: Vec<u16> = bin[value..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|u| *u != 0)
        .collect();
    let text = String::from_utf16(&units).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// [`version_string`] of the file at `path` (links followed: the game loads through them), `None`
/// when it cannot be read or is larger than any file whose version is asked for.
pub(crate) fn version_string_of_file(path: &Path, key: &str) -> Option<String> {
    let bytes = read_bounded(path)?;
    version_string(&bytes, key)
}

/// The file at `path`, up to [`MAX_VERSIONED_FILE_BYTES`]; `None` when it is larger, or no file.
pub(crate) fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    // A regular file before it is opened: opening a FIFO would wait for a writer forever.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let f = File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(MAX_VERSIONED_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    (buf.len() as u64 <= MAX_VERSIONED_FILE_BYTES).then_some(buf)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One `String` entry of a version resource, as a linker writes it (padded to 32 bits).
    pub(crate) fn entry(key: &str, value: &str) -> Vec<u8> {
        let wide = |s: &str| -> Vec<u8> {
            let mut v: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
            v.extend([0, 0]);
            v
        };
        let key = wide(key);
        let val = wide(value);
        let mut head = 6 + key.len();
        head = head.div_ceil(4) * 4;
        let length = head + val.len();
        let mut out = Vec::new();
        out.extend((length as u16).to_le_bytes());
        out.extend(((val.len() / 2) as u16).to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(&key);
        out.resize(head, 0);
        out.extend(&val);
        out.resize(out.len().div_ceil(4) * 4, 0);
        out
    }

    #[test]
    fn a_key_whose_value_needs_padding_is_read_from_the_boundary() {
        // "ProductName" (11 chars): 6 + 24 = 30 bytes, so two bytes of padding before the value.
        let mut bin = vec![0x4D, 0x5A, 1, 2, 3, 4, 5, 6];
        bin.extend(entry("ProductName", "ReShade"));
        assert_eq!(
            version_string(&bin, "ProductName").as_deref(),
            Some("ReShade")
        );
        assert_eq!(version_string(&bin, "ProductVersion"), None);
    }

    #[test]
    fn a_key_in_another_structure_is_not_a_version_string() {
        // The same UTF-16 text, but not behind a text entry's header (wType 0, or no room for one).
        let mut quoted: Vec<u8> = vec![0, 0, 0, 0, 0, 0];
        quoted.extend("ProductVersion".encode_utf16().flat_map(u16::to_le_bytes));
        quoted.extend([0, 0, b'5', 0, 0, 0]);
        assert_eq!(version_string(&quoted, "ProductVersion"), None);
        let mut at_start: Vec<u8> = "ProductVersion"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        at_start.extend([0, 0]);
        assert_eq!(version_string(&at_start, "ProductVersion"), None);
        // A value claiming to run past its entry.
        let mut e = entry("ProductVersion", "5.4.21.0");
        e[2] = 200;
        assert_eq!(version_string(&e, "ProductVersion"), None);
    }
}
