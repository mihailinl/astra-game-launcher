// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Zips written byte by byte, exactly as specified: what no writer would write, for the
//! `zip-inconsistent` checks (`src/zip_consistency.rs`) and the archives inside a release.

use std::io::Write as _;

/// The data-descriptor signature, `PK\x07\x08`.
pub const DESCRIPTOR_SIGNATURE: [u8; 4] = 0x0807_4b50u32.to_le_bytes();

/// One entry as it is written: `data` is what follows the local header (stored or deflated
/// bytes), `after` what follows `data` (a descriptor, or nothing), `before` what precedes the
/// local header (nothing, in an archive every reader reads alike).
pub struct Raw {
    pub name: Vec<u8>,
    pub method: u16,
    pub flags: u16,
    pub crc: u32,
    pub compressed: u32,
    pub size: u32,
    pub data: Vec<u8>,
    pub after: Vec<u8>,
    pub before: Vec<u8>,
    pub local_extra: Vec<u8>,
    pub central_extra: Vec<u8>,
    /// The flags of the local header, when they differ from the central record's.
    pub local_flags: Option<u16>,
}

impl Raw {
    pub fn stored(name: &str, data: &[u8]) -> Raw {
        Raw {
            name: name.as_bytes().to_vec(),
            method: 0,
            flags: 0,
            crc: crc32fast::hash(data),
            compressed: data.len() as u32,
            size: data.len() as u32,
            data: data.to_vec(),
            after: Vec::new(),
            before: Vec::new(),
            local_extra: Vec::new(),
            central_extra: Vec::new(),
            local_flags: None,
        }
    }

    /// `deflated` (which may carry bytes past its stream's end) recording `data`'s CRC and size.
    pub fn deflated(name: &str, deflated: &[u8], data: &[u8]) -> Raw {
        Raw {
            method: 8,
            compressed: deflated.len() as u32,
            size: data.len() as u32,
            data: deflated.to_vec(),
            ..Raw::stored(name, data)
        }
    }

    /// Stored with bit 3: zeros in the local header, `descriptor` after the data.
    pub fn stored_with_descriptor(name: &str, data: &[u8], descriptor: Vec<u8>) -> Raw {
        Raw {
            flags: 1 << 3,
            after: descriptor,
            ..Raw::stored(name, data)
        }
    }

    /// Stored with bit 3 and the signed descriptor a writer to a pipe writes.
    pub fn streamed(name: &str, data: &[u8]) -> Raw {
        let n = data.len() as u32;
        Raw::stored_with_descriptor(name, data, descriptor(crc32fast::hash(data), n, n))
    }
}

/// A signed descriptor with 32-bit sizes.
pub fn descriptor(crc: u32, compressed: u32, size: u32) -> Vec<u8> {
    let mut d = DESCRIPTOR_SIGNATURE.to_vec();
    for v in [crc, compressed, size] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d
}

fn le(out: &mut Vec<u8>, v16: &[u16], v32: &[u32]) {
    for v in v16 {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in v32 {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// The local header `e` writes (zeros in place of the CRC and sizes for bit 3).
pub fn local_header(e: &Raw) -> Vec<u8> {
    let mut h = Vec::new();
    let local_flags = e.local_flags.unwrap_or(e.flags);
    let (crc, compressed, size) = if e.flags & (1 << 3) != 0 {
        (0, 0, 0)
    } else {
        (e.crc, e.compressed, e.size)
    };
    le(&mut h, &[], &[0x0403_4b50]);
    le(
        &mut h,
        &[20, local_flags, e.method, 0, 0x5021],
        &[crc, compressed, size],
    );
    le(
        &mut h,
        &[e.name.len() as u16, e.local_extra.len() as u16],
        &[],
    );
    h.extend_from_slice(&e.name);
    h.extend_from_slice(&e.local_extra);
    h
}

/// The zip of `entries`, in order, then `before_directory`, the central directory (every offset
/// where its entry's local header really is) and the end record.
pub fn archive_with(entries: &[Raw], before_directory: &[u8]) -> Vec<u8> {
    let mut zip = Vec::new();
    let mut offsets = Vec::new();
    for e in entries {
        zip.extend_from_slice(&e.before);
        offsets.push(zip.len() as u32);
        zip.extend_from_slice(&local_header(e));
        zip.extend_from_slice(&e.data);
        zip.extend_from_slice(&e.after);
    }
    zip.extend_from_slice(before_directory);
    let cd_start = zip.len() as u32;
    for (e, offset) in entries.iter().zip(&offsets) {
        le(&mut zip, &[], &[0x0201_4b50]);
        le(
            &mut zip,
            &[20, 20, e.flags, e.method, 0, 0x5021],
            &[e.crc, e.compressed, e.size],
        );
        le(
            &mut zip,
            &[e.name.len() as u16, e.central_extra.len() as u16, 0, 0, 0],
            &[0, *offset],
        );
        zip.extend_from_slice(&e.name);
        zip.extend_from_slice(&e.central_extra);
    }
    let cd_len = zip.len() as u32 - cd_start;
    let n = entries.len() as u16;
    le(&mut zip, &[], &[0x0605_4b50]);
    le(&mut zip, &[0, 0, n, n], &[cd_len, cd_start]);
    le(&mut zip, &[0], &[]);
    zip
}

/// [`archive_with`], nothing before the directory.
pub fn archive(entries: &[Raw]) -> Vec<u8> {
    archive_with(entries, &[])
}

pub fn deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}
